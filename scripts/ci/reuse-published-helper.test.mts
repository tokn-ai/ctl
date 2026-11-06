import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { lstat, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { readVerifiedHelper } from "./build-ctl-bundle.mts";
import { reusePublishedHelper } from "./reuse-published-helper.mts";
import { managedLabelPrefix, type GhRunner } from "./update-draft-release.mts";

const identity = { target: "aarch64-apple-darwin", app_version: "0.1.0", git_revision: "a".repeat(40) };
const archive = `ctld-${identity.app_version}-${identity.target}.app.tar.gz`;
const manifest_name = `ctld-${identity.target}.json`;
const checksum_name = `${archive}.sha256`;
const protocols = [
  { name: "ctld", build: 12, version: "1.0.12", supported_versions: ["1.0.12"] },
  ...["ctld_lifecycle", "ctld_helper"].map((name) => ({ name, build: 1, version: "1.0.1", supported_versions: ["1.0.1"] })),
];
interface Asset { name: string; label: string | null; digest: string; size: number }

function helperFiles(content: string): Map<string, Buffer> {
  const bytes = Buffer.from(content);
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  const manifest = {
    schema_version: 1, component: "ctld", ...identity, bundle_id: identity.app_version,
    bundle_identifier: "dev.tokn-ai.ctl.ctld", team_identifier: "ABC123DEF4",
    signing_mode: "signed", notarized: true, archive, sha256, archive_size: bytes.length, protocols,
  };
  return new Map([
    [manifest_name, Buffer.from(JSON.stringify(manifest))],
    [archive, bytes],
    [checksum_name, Buffer.from(`${sha256}  ${archive}\n`)],
  ]);
}

function releaseAssets(files: Map<string, Buffer>): Asset[] {
  return [...files].map(([name, bytes]) => ({
    name, label: `${managedLabelPrefix}${name}`,
    digest: `sha256:${createHash("sha256").update(bytes).digest("hex")}`, size: bytes.length,
  }));
}

function github(files: Map<string, Buffer>, options: {
  assets?: Asset[];
  release?: "published" | "draft" | "missing";
  tag?: "missing" | "wrong";
  download_error?: boolean;
} = {}) {
  const calls: string[][] = [];
  const gh: GhRunner = async (args) => {
    calls.push(args);
    assert.ok(!args.includes("--method"), "helper reuse must never mutate GitHub");
    if (args[0] === "release") {
      assert.equal(args[1], "download");
      if (options.download_error) throw new Error("download failed");
      const directory = args[args.indexOf("--dir") + 1]!;
      for (let index = 0; index < args.length; index += 1) {
        if (args[index] === "--pattern") {
          const name = args[index + 1]!;
          assert.ok(files.has(name));
          await writeFile(join(directory, name), files.get(name)!);
        }
      }
      return "";
    }
    const endpoint = args[1]!;
    if (endpoint.includes("git/matching-refs")) {
      return JSON.stringify(options.tag === "missing" ? [] : [{
        ref: `refs/tags/v${identity.app_version}`,
        object: { type: "commit", sha: options.tag === "wrong" ? "b".repeat(40) : identity.git_revision },
      }]);
    }
    if (endpoint.endsWith("releases?per_page=100")) {
      return JSON.stringify([options.release === "missing" ? [] : [{
        id: 42, tag_name: `v${identity.app_version}`, draft: options.release === "draft",
        body: "Release notes", html_url: "https://github.com/tokn-ai/ctl/releases/tag/v0.1.0",
      }]]);
    }
    if (endpoint.endsWith("/assets?per_page=100")) return JSON.stringify([options.assets ?? releaseAssets(files)]);
    throw new Error(`Unexpected GitHub call: ${args.join(" ")}`);
  };
  return { gh, calls };
}

async function fixture(t: TestContext) {
  const directory = await mkdtemp(join(tmpdir(), "ctl-reuse-helper-test-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const original = helperFiles("newly signed helper bytes");
  for (const [name, bytes] of original) await writeFile(join(directory, name), bytes);
  const unchanged = async () => {
    for (const [name, bytes] of original) assert.deepEqual(await readFile(join(directory, name)), bytes);
  };
  return { directory, unchanged };
}

test("restores all canonical helper bytes before the CLI reads its embedded helper", async (t) => {
  const { directory } = await fixture(t);
  const published = helperFiles("previously signed and published helper bytes");
  const api = github(published);
  assert.equal(await reusePublishedHelper("tokn-ai/ctl", identity, directory, api.gh), true);
  for (const [name, bytes] of published) assert.deepEqual(await readFile(join(directory, name)), bytes);
  const helper = await readVerifiedHelper(directory, identity);
  assert.equal(helper.sha256, JSON.parse(published.get(manifest_name)!.toString()).sha256);
  const download = api.calls.find((args) => args[0] === "release")!;
  await assert.rejects(lstat(download[download.indexOf("--dir") + 1]!), { code: "ENOENT" });
});

test("leaves a new helper unchanged when no same-target published helper exists", async (t) => {
  for (const options of [
    { release: "missing" as const }, { release: "draft" as const }, { assets: [] },
    { assets: [{ name: "ctld-x86_64-apple-darwin.json", label: null, digest: "unused", size: 1 }] },
  ]) {
    await t.test(JSON.stringify(options), async (t) => {
      const { directory, unchanged } = await fixture(t);
      const api = github(helperFiles("published"), options);
      assert.equal(await reusePublishedHelper("tokn-ai/ctl", identity, directory, api.gh), false);
      await unchanged();
      assert.ok(!api.calls.some((args) => args[0] === "release"));
    });
  }
});

test("requires the exact version tag and validates arguments before modifying local files", async (t) => {
  for (const tag of ["missing", "wrong"] as const) {
    await t.test(tag, async (t) => {
      const { directory, unchanged } = await fixture(t);
      const api = github(helperFiles("published"), { tag });
      await assert.rejects(reusePublishedHelper("tokn-ai/ctl", identity, directory, api.gh), /does not exist|points to/);
      await unchanged();
      assert.equal(api.calls.length, 1);
    });
  }
  const { directory, unchanged } = await fixture(t);
  for (const change of [{ app_version: "main" }, { target: "unknown" }, { git_revision: "invalid" }]) {
    const api = github(helperFiles("published"));
    await assert.rejects(reusePublishedHelper("tokn-ai/ctl", { ...identity, ...change }, directory, api.gh), /Expected a macOS/);
    assert.equal(api.calls.length, 0);
  }
  await unchanged();
});

test("rejects partial, duplicate, manually managed, or invalid asset records before downloading", async (t) => {
  const files = helperFiles("published");
  const complete = releaseAssets(files);
  const cases = [
    ...complete.map((_, index) => complete.filter((_, other) => index !== other)),
    [...complete, complete[0]!],
    complete.map((asset, index) => index === 0 ? { ...asset, label: null } : asset),
    complete.map((asset, index) => index === 0 ? { ...asset, digest: "missing" } : asset),
    complete.map((asset, index) => index === 0 ? { ...asset, size: 0 } : asset),
    complete.map((asset) => asset.name === archive ? { ...asset, size: 128 * 1024 * 1024 + 1 } : asset),
  ];
  for (const [index, assets] of cases.entries()) {
    await t.test(`invalid record ${index}`, async (t) => {
      const { directory, unchanged } = await fixture(t);
      const api = github(files, { assets });
      await assert.rejects(reusePublishedHelper("tokn-ai/ctl", identity, directory, api.gh), /incomplete|manually managed|invalid digest or size/);
      await unchanged();
      assert.ok(!api.calls.some((args) => args[0] === "release"));
    });
  }
});

test("rejects corrupt downloads and validates identity, protocols, archive, and checksum before replacing files", async (t) => {
  const changes: Array<[string, (files: Map<string, Buffer>) => Asset[] | undefined]> = [
    ["digest", (files) => releaseAssets(files).map((asset, index) => index === 0 ? { ...asset, digest: `sha256:${"0".repeat(64)}` } : asset)],
    ["size", (files) => releaseAssets(files).map((asset, index) => index === 0 ? { ...asset, size: asset.size + 1 } : asset)],
    ["identity", (files) => {
      const manifest = JSON.parse(files.get(manifest_name)!.toString());
      files.set(manifest_name, Buffer.from(JSON.stringify({ ...manifest, git_revision: "b".repeat(40) })));
      return undefined;
    }],
    ["protocols", (files) => {
      const manifest = JSON.parse(files.get(manifest_name)!.toString());
      files.set(manifest_name, Buffer.from(JSON.stringify({ ...manifest, protocols: [] })));
      return undefined;
    }],
    ["archive", (files) => { files.set(archive, Buffer.from("corrupt archive")); return undefined; }],
    ["checksum", (files) => { files.set(checksum_name, Buffer.from("incorrect checksum")); return undefined; }],
  ];
  for (const [name, change] of changes) {
    await t.test(name, async (t) => {
      const { directory, unchanged } = await fixture(t);
      const files = helperFiles("published");
      const api = github(files, { assets: change(files) });
      await assert.rejects(reusePublishedHelper("tokn-ai/ctl", identity, directory, api.gh));
      await unchanged();
      const download = api.calls.find((args) => args[0] === "release")!;
      await assert.rejects(lstat(download[download.indexOf("--dir") + 1]!), { code: "ENOENT" });
    });
  }
});

test("download failures preserve the new helper and remove temporary files", async (t) => {
  const { directory, unchanged } = await fixture(t);
  const api = github(helperFiles("published"), { download_error: true });
  await assert.rejects(reusePublishedHelper("tokn-ai/ctl", identity, directory, api.gh), /download failed/);
  await unchanged();
  const download = api.calls.find((args) => args[0] === "release")!;
  await assert.rejects(lstat(download[download.indexOf("--dir") + 1]!), { code: "ENOENT" });
});
