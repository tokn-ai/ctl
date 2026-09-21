import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import {
  planAssetUpdate,
  releaseNotes,
  updateDraftRelease,
  validateReleaseBundle,
  type BundleIdentity,
  type GhRunner,
  type ReleaseBundle,
} from "./update-draft-release.mts";

const identity: BundleIdentity = {
  app_version: "0.1.0",
  bundle_id: "0.1.0-dev.0123456789ab",
  git_revision: "0123456789abcdef0123456789abcdef01234567",
};
const desktopTargets = [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
];
const remoteTargets = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
];

async function fixture(t: TestContext): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), "rmux-release-test-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const addAsset = async (name: string): Promise<string> => {
    const content = `test bundle ${name}`;
    const sha256 = createHash("sha256").update(content).digest("hex");
    await writeFile(join(directory, name), content);
    await writeFile(join(directory, `${name}.sha256`), `${sha256}  ${name}\n`);
    return sha256;
  };
  const targets: Record<string, { archive: string; sha256: string }> = {};
  for (const target of remoteTargets) {
    const archive = `ctl-agent-bundle-${identity.bundle_id}-${target}.tar.gz`;
    targets[target] = { archive, sha256: await addAsset(archive) };
  }
  await writeFile(join(directory, "bundle-set.json"), JSON.stringify({
    schema_version: 1, ...identity, targets,
  }));
  for (const target of desktopTargets) {
    const extensions = target.endsWith("apple-darwin") ? ["dmg", "app.tar.gz"] : ["deb", "rpm", "AppImage"];
    const assets: { name: string; sha256: string }[] = [];
    for (const extension of extensions) {
      const name = `rmux-${identity.bundle_id}-${target}.${extension}`;
      assets.push({ name, sha256: await addAsset(name) });
    }
    await writeFile(join(directory, `desktop-${target}.json`), JSON.stringify({
      schema_version: 1,
      target,
      bundle_id: identity.bundle_id,
      git_revision: identity.git_revision,
      signing_mode: target.endsWith("apple-darwin") ? "unsigned" : "not_applicable",
      assets,
    }));
  }
  return directory;
}

const exampleBundle: ReleaseBundle = {
  ...identity,
  asset_directory: "/tmp/bundles",
  asset_names: ["new-package.dmg", "desktop.json"],
  unsigned_targets: ["aarch64-apple-darwin"],
};

interface MockRelease {
  id: number;
  tag_name: string;
  draft: boolean;
  body: string | null;
  html_url: string;
}

function mockGithub(options: {
  release?: MockRelease | null;
  assets?: { id: number; name: string; label: string | null }[];
  upload_error?: boolean;
  latest_body?: string;
  published_before_upload?: boolean;
} = {}): {
  gh: GhRunner;
  calls: string[][];
  payloads: Record<string, unknown>[];
} {
  const calls: string[][] = [];
  const payloads: Record<string, unknown>[] = [];
  let release = options.release === undefined ? {
    id: 42,
    tag_name: "v0.1.0",
    draft: true,
    body: "Handwritten release notes.",
    html_url: "https://github.com/tokn-ai/ctl/releases/tag/v0.1.0",
  } : options.release;
  const gh: GhRunner = async (args) => {
    calls.push(args);
    if (args[0] === "api" && args[1] === "repos/tokn-ai/ctl/releases?per_page=100") {
      return JSON.stringify([[{ id: 1, tag_name: "v0.0.1", draft: false }], release ? [release] : []]);
    }
    if (args[0] === "release" && args[1] === "create") {
      assert.ok(args.includes("--draft"));
      assert.equal(args[args.indexOf("--target") + 1], identity.git_revision);
      release = {
        id: 42, tag_name: "v0.1.0", draft: true,
        body: await readFile(args[args.indexOf("--notes-file") + 1]!, "utf8"),
        html_url: "https://github.com/tokn-ai/ctl/releases/tag/v0.1.0",
      };
      return release.html_url;
    }
    if (args[1] === "repos/tokn-ai/ctl/releases/42/assets?per_page=100") {
      return JSON.stringify([options.assets ?? []]);
    }
    if (args[0] === "release" && args[1] === "upload") {
      if (options.upload_error) {
        throw new Error("Upload failed");
      }
      return "";
    }
    if (args[1] === "repos/tokn-ai/ctl/releases/42") {
      if (args.includes("PATCH")) {
        payloads.push(JSON.parse(await readFile(args[args.indexOf("--input") + 1]!, "utf8")));
        return "{}";
      }
      return JSON.stringify({
        ...release,
        ...(options.latest_body === undefined ? {} : { body: options.latest_body }),
        ...(options.published_before_upload ? { draft: false } : {}),
      });
    }
    if (args.includes("DELETE")) {
      return "";
    }
    throw new Error(`Unexpected gh invocation: ${args.join(" ")}`);
  };
  return { gh, calls, payloads };
}

test("validates all four desktop and remote targets and their checksum files", async (t) => {
  const directory = await fixture(t);
  const bundle = await validateReleaseBundle(identity, directory);
  assert.equal(bundle.asset_names.length, 33);
  assert.deepEqual(bundle.unsigned_targets, ["x86_64-apple-darwin", "aarch64-apple-darwin"]);
  assert.ok(bundle.asset_names.includes("bundle-set.json"));
});

test("refuses an incomplete target set", async (t) => {
  const directory = await fixture(t);
  await rm(join(directory, "desktop-aarch64-apple-darwin.json"));
  await assert.rejects(validateReleaseBundle(identity, directory), /ENOENT/);
});

test("rejects missing desktop formats and filenames from a different build", async (t) => {
  await t.test("missing package format", async (t) => {
    const directory = await fixture(t);
    const path = join(directory, "desktop-x86_64-unknown-linux-gnu.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.assets.pop();
    await writeFile(path, JSON.stringify(manifest));
    await assert.rejects(validateReleaseBundle(identity, directory), /every required package format/);
  });
  await t.test("different build filename", async (t) => {
    const directory = await fixture(t);
    const path = join(directory, "desktop-aarch64-apple-darwin.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.assets[0].name = "rmux-old-build-aarch64-apple-darwin.dmg";
    await writeFile(path, JSON.stringify(manifest));
    await assert.rejects(validateReleaseBundle(identity, directory), /Unexpected desktop package filename/);
  });
});

test("rejects mixed build identities, corrupt assets, and unexpected files", async (t) => {
  await t.test("different remote identity", async (t) => {
    const directory = await fixture(t);
    await assert.rejects(validateReleaseBundle({ ...identity, bundle_id: "other" }, directory), /identity/);
  });
  await t.test("different desktop identity", async (t) => {
    const directory = await fixture(t);
    const path = join(directory, "desktop-aarch64-apple-darwin.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.git_revision = "a".repeat(40);
    await writeFile(path, JSON.stringify(manifest));
    await assert.rejects(validateReleaseBundle(identity, directory), /identity/);
  });
  await t.test("corrupt package", async (t) => {
    const directory = await fixture(t);
    await writeFile(join(directory, `ctl-agent-bundle-${identity.bundle_id}-${remoteTargets[0]}.tar.gz`), "changed");
    await assert.rejects(validateReleaseBundle(identity, directory), /Checksum mismatch/);
  });
  await t.test("unverified checksum file", async (t) => {
    const directory = await fixture(t);
    await writeFile(join(directory, `ctl-agent-bundle-${identity.bundle_id}-${remoteTargets[0]}.tar.gz.sha256`), "bad");
    await assert.rejects(validateReleaseBundle(identity, directory), /Invalid checksum file/);
  });
  await t.test("unexpected attachment", async (t) => {
    const directory = await fixture(t);
    await writeFile(join(directory, "unexpected.zip"), "unexpected");
    await assert.rejects(validateReleaseBundle(identity, directory), /Unexpected file/);
  });
});

test("rejects traversal, symlinks, and invalid signing modes", async (t) => {
  await t.test("path traversal", async (t) => {
    const directory = await fixture(t);
    const path = join(directory, "desktop-aarch64-apple-darwin.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.assets[0].name = "../outside.dmg";
    await writeFile(path, JSON.stringify(manifest));
    await assert.rejects(validateReleaseBundle(identity, directory), /Unsafe asset filename/);
  });
  await t.test("symlink manifest", async (t) => {
    const directory = await fixture(t);
    const path = join(directory, "desktop-aarch64-apple-darwin.json");
    await rm(path);
    await symlink(join(directory, "desktop-x86_64-apple-darwin.json"), path);
    await assert.rejects(validateReleaseBundle(identity, directory), /regular file/);
  });
  await t.test("invalid signing mode", async (t) => {
    const directory = await fixture(t);
    const path = join(directory, "desktop-aarch64-apple-darwin.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.signing_mode = "not_applicable";
    await writeFile(path, JSON.stringify(manifest));
    await assert.rejects(validateReleaseBundle(identity, directory), /macOS signing mode/);
  });
});

test("replaces only generated notes and describes unsigned macOS limitations", () => {
  const old = "User introduction.\n<!-- rmux-ci:start -->old build<!-- rmux-ci:end -->\nUser changelog.";
  const updated = releaseNotes(exampleBundle, old);
  assert.ok(updated.startsWith("User introduction.\n"));
  assert.ok(updated.endsWith("\nUser changelog."));
  assert.ok(updated.includes(identity.git_revision));
  assert.ok(updated.includes("not Developer ID signed or notarized"));
  assert.ok(updated.includes("Touch ID credential storage is unavailable"));
  assert.ok(!updated.includes("old build"));
  assert.equal(releaseNotes(exampleBundle, updated), updated);
  assert.throws(() => releaseNotes(exampleBundle, "<!-- rmux-ci:start -->incomplete"), /malformed/);
});

test("existing draft uploads managed assets, preserves manual assets and notes, then cleans up", async () => {
  const github = mockGithub({
    assets: [
      { id: 1, name: "obsolete.dmg", label: "rmux-ci: obsolete.dmg" },
      { id: 2, name: "manual.pdf", label: null },
      { id: 3, name: "desktop.json", label: "rmux-ci: desktop.json" },
    ],
    latest_body: "Notes edited while the upload ran.",
  });
  const result = await updateDraftRelease("tokn-ai/ctl", exampleBundle, github.gh);
  assert.equal(result.status, "updated");
  assert.equal(result.html_url, "https://github.com/tokn-ai/ctl/releases/tag/v0.1.0");
  const upload = github.calls.findIndex((args) => args[0] === "release" && args[1] === "upload");
  const patch = github.calls.findIndex((args) => args.includes("PATCH"));
  const deletion = github.calls.findIndex((args) => args.includes("DELETE"));
  assert.ok(upload > 0 && patch > upload && deletion > patch);
  assert.ok(github.calls[upload]!.includes("--clobber"));
  assert.ok(github.calls[upload]!.includes("/tmp/bundles/new-package.dmg#rmux-ci: new-package.dmg"));
  assert.deepEqual(github.calls.filter((args) => args.includes("DELETE")), [
    ["api", "repos/tokn-ai/ctl/releases/assets/1", "--method", "DELETE"],
  ]);
  assert.equal(github.payloads[0]!.name, "rmux v0.1.0");
  assert.equal(github.payloads[0]!.target_commitish, identity.git_revision);
  assert.ok(String(github.payloads[0]!.body).startsWith("Notes edited while the upload ran."));
  assert.ok(!("draft" in github.payloads[0]!));
});

test("creates a missing release as a draft at the exact build revision", async () => {
  const github = mockGithub({ release: null });
  assert.equal((await updateDraftRelease("tokn-ai/ctl", exampleBundle, github.gh)).status, "updated");
  const creation = github.calls.find((args) => args[0] === "release" && args[1] === "create")!;
  assert.ok(creation.includes("--draft"));
  assert.equal(creation[creation.indexOf("--target") + 1], identity.git_revision);
});

test("leaves a published release unchanged", async () => {
  const github = mockGithub({ release: {
    id: 42, tag_name: "v0.1.0", draft: false, body: "Published notes",
    html_url: "https://github.com/tokn-ai/ctl/releases/tag/v0.1.0",
  } });
  assert.equal((await updateDraftRelease("tokn-ai/ctl", exampleBundle, github.gh)).status, "published");
  assert.equal(github.calls.length, 1);
});

test("rechecks draft status before uploading", async () => {
  const github = mockGithub({ published_before_upload: true });
  assert.equal((await updateDraftRelease("tokn-ai/ctl", exampleBundle, github.gh)).status, "published");
  assert.ok(!github.calls.some((args) => args[0] === "release" || args.includes("PATCH") || args.includes("DELETE")));
});

test("upload failure preserves obsolete assets and previous metadata", async () => {
  const github = mockGithub({
    upload_error: true,
    assets: [{ id: 1, name: "previous.dmg", label: "rmux-ci: previous.dmg" }],
  });
  await assert.rejects(updateDraftRelease("tokn-ai/ctl", exampleBundle, github.gh), /Upload failed/);
  assert.ok(!github.calls.some((args) => args.includes("DELETE") || args.includes("PATCH")));
});

test("rejects a manual attachment collision before uploading anything", async () => {
  const github = mockGithub({ assets: [{ id: 2, name: "desktop.json", label: null }] });
  await assert.rejects(updateDraftRelease("tokn-ai/ctl", exampleBundle, github.gh), /manually managed/);
  assert.ok(!github.calls.some((args) => args[0] === "release" || args.includes("DELETE") || args.includes("PATCH")));
  assert.deepEqual(planAssetUpdate([
    { id: 1, name: "manual.pdf", label: "" },
    { id: 2, name: "old.dmg", label: "rmux-ci: old.dmg" },
  ], []), [{ id: 2, name: "old.dmg", label: "rmux-ci: old.dmg" }]);
});
