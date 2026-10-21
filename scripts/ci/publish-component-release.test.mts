import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFile, cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import { componentMapFixture } from "./agent-bundle.fixtures.mts";
import { componentSetFixture, publicationFixture } from "./bundle-publication.fixtures.mts";
import { fileDigest, validatedComponentSet } from "./bundle-publication.mts";
import { publishComponentRelease } from "./publish-component-release.mts";
import { managedLabelPrefix, type GhRunner } from "./update-draft-release.mts";

const identity = publicationFixture(1, "stable");
interface Asset { id: number; name: string; label: string | null; digest: string; size: number }
interface Release { id: number; tag_name: string; draft: boolean; body: string | null; html_url: string }

async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ctl-publish-release-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

function github(options: { release?: Release | null; assets?: Asset[]; canonical_directory?: string; tag?: string; bad_tag?: boolean; missing_tag?: boolean; fail_upload?: boolean; bad_digest?: boolean } = {}) {
  let release: Release | null = options.release ?? null;
  const assets = [...(options.assets ?? [])];
  const calls: string[][] = [], patches: Record<string, unknown>[] = [];
  const gh: GhRunner = async (args) => {
    calls.push(args);
    if (args[0] === "release") {
      assert.ok(options.canonical_directory);
      const directory = args[args.indexOf("--dir") + 1]!;
      await mkdir(directory, { recursive: true });
      for (let index = 0; index < args.length; index += 1) if (args[index] === "--pattern") await copyFile(join(options.canonical_directory, args[index + 1]!), join(directory, args[index + 1]!));
      return "";
    }
    const endpoint = args[1]!, method = args[args.indexOf("--method") + 1];
    if (endpoint.includes("git/matching-refs")) return JSON.stringify(options.missing_tag ? [] : [{ ref: `refs/tags/${options.tag ?? `v${identity.app_version}`}`, object: { type: "commit", sha: options.bad_tag ? "b".repeat(40) : identity.git_revision } }]);
    if (endpoint.endsWith("releases?per_page=100")) return JSON.stringify([release ? [release] : []]);
    if (endpoint.endsWith("/releases") && method === "POST") {
      const payload = JSON.parse(await readFile(args[args.indexOf("--input") + 1]!, "utf8"));
      release = { id: 42, tag_name: payload.tag_name, draft: payload.draft, body: payload.body, html_url: "https://github.com/tokn-ai/ctl/releases/tag/v0.1.0" };
      return JSON.stringify(release);
    }
    if (endpoint.endsWith("/assets?per_page=100")) return JSON.stringify([assets]);
    if (endpoint.startsWith("https://uploads.github.com/")) {
      if (options.fail_upload) throw new Error("upload failed");
      const name = new URL(endpoint).searchParams.get("name")!;
      const path = args[args.indexOf("--input") + 1]!;
      const asset = { id: assets.length + 100, name, label: `${managedLabelPrefix}${name}`, digest: options.bad_digest ? "sha256:wrong" : await fileDigest(path), size: (await readFile(path)).length };
      assets.push(asset);
      return JSON.stringify({ ...asset, state: "uploaded" });
    }
    if (endpoint.includes("/releases/assets/") && method === "DELETE") {
      const index = assets.findIndex((asset) => asset.id === Number(endpoint.split("/").at(-1)));
      assets.splice(index, 1);
      return "";
    }
    if (endpoint.endsWith("/releases/42") && method === "PATCH") {
      const patch = JSON.parse(await readFile(args[args.indexOf("--input") + 1]!, "utf8"));
      patches.push(patch);
      release = { ...release!, ...patch };
      return JSON.stringify(release);
    }
    if (endpoint.endsWith("/releases/42")) return JSON.stringify(release);
    throw new Error(`Unexpected GitHub call: ${args.join(" ")}`);
  };
  return { gh, calls, patches, assets, release: () => release };
}

const published: Release = { id: 42, tag_name: "v0.1.0", draft: false, body: "Maintainer notes", html_url: "https://github.com/tokn-ai/ctl/releases/tag/v0.1.0" };

async function existingComponents(directory: string): Promise<Asset[]> {
  const { asset_names } = await validatedComponentSet(directory, identity);
  return Promise.all(asset_names.map(async (name, id) => ({ id, name, label: `${managedLabelPrefix}${name}`, digest: await fileDigest(join(directory, name)), size: (await readFile(join(directory, name))).length })));
}

test("publishes verified components without desktop artifacts and preserves manual assets/notes", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  const api = github({ release: { ...published, draft: true }, assets: [
    { id: 1, name: "old-development.dmg", label: `${managedLabelPrefix}old-development.dmg`, digest: "old", size: 1 },
    { id: 2, name: "manual.pdf", label: null, digest: "manual", size: 1 },
  ] });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "components", api.gh);
  assert.equal(api.release()?.draft, false);
  assert.ok(api.release()?.body?.startsWith("Maintainer notes"));
  assert.equal(api.assets.length, 10);
  assert.ok(api.assets.some((asset) => asset.name === "manual.pdf"));
  assert.ok(!api.assets.some((asset) => asset.name === "old-development.dmg"));
  const last_upload = api.calls.map((args) => args[1]!.startsWith("https://uploads.github.com")).lastIndexOf(true);
  const deletion = api.calls.findIndex((args) => args.includes("DELETE"));
  const publication = api.calls.findIndex((args) => args.includes("PATCH"));
  assert.ok(last_upload < deletion && deletion < publication);
});

test("a new prerelease publishes all components without becoming the latest release", async (t) => {
  const prerelease = publicationFixture(1, "stable", "0.1.0-beta.1");
  const directory = await componentSetFixture(await fixture(t), prerelease);
  const api = github({ tag: `v${prerelease.app_version}` });
  await publishComponentRelease("tokn-ai/ctl", prerelease, directory, "components", api.gh);
  assert.equal(api.release()?.draft, false);
  assert.equal(api.release()?.tag_name, "v0.1.0-beta.1");
  assert.equal(api.assets.length, 9);
  assert.equal(api.patches[0]?.prerelease, true);
  assert.equal(api.patches[0]?.make_latest, "false");
  assert.ok(api.calls.findIndex((args) => args.includes("PATCH")) > api.calls.map((args) => args[1]!.startsWith("https://uploads.github.com")).lastIndexOf(true));
});

test("failed or mismatched uploads remain drafts and leave obsolete draft assets intact", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  for (const failure of [{ fail_upload: true }, { bad_digest: true }]) {
    const api = github({ release: { ...published, draft: true }, assets: [{ id: 1, name: "old.dmg", label: `${managedLabelPrefix}old.dmg`, digest: "old", size: 1 }], ...failure });
    await assert.rejects(publishComponentRelease("tokn-ai/ctl", identity, directory, "components", api.gh), /upload failed|complete matching release asset/);
    assert.equal(api.release()?.draft, true);
    assert.ok(api.assets.some((asset) => asset.name === "old.dmg"));
    assert.ok(!api.calls.some((args) => args.includes("PATCH")));
  }
});

test("published component bytes are immutable and an identical retry performs no mutations", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  const assets = await existingComponents(directory);
  const api = github({ release: published, assets, canonical_directory: directory });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "components", api.gh);
  assert.ok(!api.calls.some((args) => args.includes("POST") || args.includes("PATCH") || args.includes("DELETE")));
  const mismatch = github({ release: published, canonical_directory: directory, assets: assets.map((asset, index) => index === 1 ? { ...asset, digest: "sha256:wrong" } : asset) });
  await assert.rejects(publishComponentRelease("tokn-ai/ctl", identity, directory, "components", mismatch.gh), /immutable digest/);
  assert.ok(!mismatch.calls.some((args) => args.includes("POST") || args.includes("PATCH") || args.includes("DELETE")));
});

test("missing/moved release tags and manual collisions cannot publish", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  const bad_tag = github({ bad_tag: true });
  await assert.rejects(publishComponentRelease("tokn-ai/ctl", identity, directory, "components", bad_tag.gh), /points to/);
  assert.ok(!bad_tag.calls.some((args) => args.includes("POST")));
  await assert.rejects(publishComponentRelease("tokn-ai/ctl", identity, directory, "components", github({ missing_tag: true }).gh), /does not exist/);
  const manual = github({ release: { ...published, draft: true }, assets: [{ id: 1, name: "bundle-set.json", label: null, digest: "manual", size: 1 }] });
  await assert.rejects(publishComponentRelease("tokn-ai/ctl", identity, directory, "components", manual.gh), /manually managed/);
  assert.ok(!manual.calls.some((args) => args.includes("POST")));
});

test("a tag rebuild reuses published canonical archives despite different valid tar metadata", async (t) => {
  const root = await fixture(t), canonical = await componentSetFixture(root, identity), rebuilt = join(root, "rebuilt");
  await cp(canonical, rebuilt, { recursive: true });
  const manifest = JSON.parse(await readFile(join(rebuilt, "bundle-set.json"), "utf8"));
  const target = manifest.targets["x86_64-apple-darwin"];
  const archive = await readFile(join(rebuilt, target.archive));
  archive.writeUInt32LE(12345678, 4);
  target.sha256 = createHash("sha256").update(archive).digest("hex");
  await writeFile(join(rebuilt, target.archive), archive);
  await writeFile(join(rebuilt, `${target.archive}.sha256`), `${target.sha256}  ${target.archive}\n`);
  await writeFile(join(rebuilt, "bundle-set.json"), JSON.stringify(manifest));
  await validatedComponentSet(rebuilt, identity);
  const api = github({ release: published, assets: await existingComponents(canonical), canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, rebuilt, "components", api.gh);
  assert.deepEqual(await readFile(join(rebuilt, target.archive)), await readFile(join(canonical, target.archive)));
  assert.deepEqual(await readFile(join(rebuilt, "bundle-set.json")), await readFile(join(canonical, "bundle-set.json")));
  assert.ok(!api.calls.some((args) => args.includes("POST") || args.includes("PATCH") || args.includes("DELETE")));
});

test("desktop packages append to an already-published matching component release", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  const api = github({ release: published, assets: await existingComponents(directory), canonical_directory: directory });
  const write_asset = async (name: string) => {
    const bytes = Buffer.from(`fixture ${name}`), sha256 = createHash("sha256").update(bytes).digest("hex");
    await writeFile(join(directory, name), bytes);
    await writeFile(join(directory, `${name}.sha256`), `${sha256}  ${name}\n`);
    return { archive: name, sha256, archive_size: bytes.length };
  };
  for (const target of ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-apple-darwin", "aarch64-apple-darwin"]) {
    const mac = target.endsWith("apple-darwin");
    const assets = [];
    for (const extension of mac ? ["dmg", "app.tar.gz"] : ["deb", "rpm", "AppImage"]) {
      const asset = await write_asset(`ctmux-${identity.bundle_id}-${target}.${extension}`);
      assets.push({ name: asset.archive, sha256: asset.sha256 });
    }
    await writeFile(join(directory, `desktop-${target}.json`), JSON.stringify({ schema_version: 1, target, bundle_id: identity.bundle_id, git_revision: identity.git_revision, signing_mode: mac ? "signed" : "not_applicable", assets }));
    if (!mac) continue;
    const helper = await write_asset(`ctld-${identity.bundle_id}-${target}.app.tar.gz`);
    const common = { schema_version: 1, app_version: identity.app_version, bundle_id: identity.bundle_id, git_revision: identity.git_revision, target, team_identifier: "ABC123DEF4", signing_mode: "signed", notarized: true };
    await writeFile(join(directory, `ctld-${target}.json`), JSON.stringify({ ...common, component: "ctld", bundle_identifier: "dev.tokn-ai.ctl.ctld", protocols: componentMapFixture(identity).ctld.protocols, ...helper }));
    await writeFile(join(directory, `ctl-cli-${target}.json`), JSON.stringify({ ...common, component: "ctl-cli", bundled_ctld_sha256: helper.sha256, ...await write_asset(`ctl-${identity.bundle_id}-${target}.tar.gz`) }));
  }
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);
  assert.equal(api.release()?.draft, false);
  assert.ok(api.release()?.body?.startsWith("Maintainer notes"));
  assert.ok(api.assets.some((asset) => asset.name === "desktop-aarch64-apple-darwin.json"));
  assert.ok(api.assets.some((asset) => asset.name === "ctl-cli-aarch64-apple-darwin.json"));
  assert.ok(!api.calls.some((args) => args.includes("DELETE")));
  assert.ok(!("draft" in api.patches[0]!));
  assert.equal(api.calls.filter((args) => args[1]?.startsWith("https://uploads.github.com") && new URL(args[1]).searchParams.get("name") === "bundle-set.json").length, 0);
  const rebuilt = join(dirname(directory), "desktop-rebuilt");
  await cp(directory, rebuilt, { recursive: true });
  const changed = "ctmux-0.1.0-x86_64-apple-darwin.dmg";
  const bytes = Buffer.from("a fresh non-reproducible signed package"), sha256 = createHash("sha256").update(bytes).digest("hex");
  await writeFile(join(rebuilt, changed), bytes);
  await writeFile(join(rebuilt, `${changed}.sha256`), `${sha256}  ${changed}\n`);
  const manifest_path = join(rebuilt, "desktop-x86_64-apple-darwin.json");
  const manifest = JSON.parse(await readFile(manifest_path, "utf8"));
  manifest.assets.find((asset: { name: string }) => asset.name === changed).sha256 = sha256;
  await writeFile(manifest_path, JSON.stringify(manifest));
  const retry = github({ release: api.release()!, assets: api.assets, canonical_directory: directory });
  await publishComponentRelease("tokn-ai/ctl", identity, rebuilt, "desktop", retry.gh);
  assert.deepEqual(await readFile(join(rebuilt, changed)), await readFile(join(directory, changed)));
  assert.ok(!retry.calls.some((args) => args.includes("POST") || args.includes("PATCH") || args.includes("DELETE")));
});
