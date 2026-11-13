import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFile, cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import { windowsAgentManifestName, windowsAgentTarget } from "../shared/agent-bundle.mts";
import { componentMapFixture } from "./agent-bundle.fixtures.mts";
import { componentSetFixture, publicationFixture, windowsComponentSetFixture } from "./bundle-publication.fixtures.mts";
import { fileDigest, validatedComponentSet } from "./bundle-publication.mts";
import { publishComponentRelease } from "./publish-component-release.mts";
import { managedLabelPrefix, releaseNotes, validateReleaseBundle, type GhRunner } from "./update-draft-release.mts";
import { updaterBinaryName, updaterManifestName, windowsUpdaterTargets } from "../shared/updater-bundle.mts";

const identity = publicationFixture(1, "stable");
const buildIdentity = { app_version: identity.app_version, bundle_id: identity.bundle_id, git_revision: identity.git_revision };
interface Asset { id: number; name: string; label: string | null; digest: string; size: number }
interface Release { id: number; tag_name: string; draft: boolean; body: string | null; html_url: string }

async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ctl-publish-release-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

function github(options: { release?: Release | null; assets?: Asset[]; canonical_directory?: string; tag?: string; bad_tag?: boolean; missing_tag?: boolean; fail_upload?: boolean; fail_after_upload?: number; bad_digest?: boolean } = {}) {
  let release: Release | null = options.release ?? null;
  let uploaded = 0;
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
      if (options.fail_upload || (options.fail_after_upload !== undefined && uploaded >= options.fail_after_upload)) throw new Error("upload failed");
      const name = new URL(endpoint).searchParams.get("name")!;
      const path = args[args.indexOf("--input") + 1]!;
      const asset = { id: assets.length + 100, name, label: `${managedLabelPrefix}${name}`, digest: options.bad_digest ? "sha256:wrong" : await fileDigest(path), size: (await readFile(path)).length };
      assets.push(asset);
      uploaded += 1;
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

async function existingReleaseAssets(directory: string): Promise<Asset[]> {
  const { asset_names } = await validateReleaseBundle(buildIdentity, directory, { allow_partial: true });
  return Promise.all(asset_names.map(async (name, id) => ({ id, name, label: `${managedLabelPrefix}${name}`, digest: await fileDigest(join(directory, name)), size: (await readFile(join(directory, name))).length })));
}

async function writeAsset(directory: string, name: string, content = `fixture ${name}`) {
  const bytes = Buffer.from(content), sha256 = createHash("sha256").update(bytes).digest("hex");
  await writeFile(join(directory, name), bytes);
  await writeFile(join(directory, `${name}.sha256`), `${sha256}  ${name}\n`);
  return { archive: name, sha256, archive_size: bytes.length };
}

async function desktopAssets(directory: string, target: string, unsigned = false): Promise<void> {
  const mac = target.endsWith("apple-darwin"), assets = [];
  const windows = target.endsWith("-pc-windows-msvc");
  const variant = unsigned ? `${target}-unsigned` : target;
  for (const extension of windows ? ["zip"] : mac ? ["dmg", "app.tar.gz"] : ["deb", "rpm", "AppImage"]) {
    const asset = await writeAsset(directory, `ctmux-${identity.bundle_id}-${variant}.${extension}`);
    assets.push({ name: asset.archive, sha256: asset.sha256 });
  }
  await writeFile(join(directory, `desktop-${variant}.json`), JSON.stringify({
    schema_version: 1, target, bundle_id: identity.bundle_id, git_revision: identity.git_revision,
    signing_mode: unsigned ? "unsigned" : mac ? "signed" : "not_applicable", assets,
  }));
  if (windows) {
    const binary = updaterBinaryName(identity, target);
    const asset = await writeAsset(directory, binary);
    await writeFile(join(directory, updaterManifestName(target)), JSON.stringify({
      schema_version: 1, ...buildIdentity, target_triple: target, binary, sha256: asset.sha256, size: asset.archive_size,
    }));
  }
}

async function helperAssets(directory: string, target: string, include_cli = false): Promise<void> {
  const helper = await writeAsset(directory, `ctld-${identity.bundle_id}-${target}.app.tar.gz`);
  const common = { schema_version: 1, app_version: identity.app_version, bundle_id: identity.bundle_id, git_revision: identity.git_revision,
    target, team_identifier: "ABC123DEF4", signing_mode: "signed", notarized: true };
  await writeFile(join(directory, `ctld-${target}.json`), JSON.stringify({ ...common, component: "ctld", bundle_identifier: "dev.tokn-ai.ctl.ctld",
    protocols: componentMapFixture(identity).ctld.protocols, ...helper }));
  if (include_cli) await writeFile(join(directory, `ctl-cli-${target}.json`), JSON.stringify({ ...common, component: "ctl-cli",
    bundled_ctld_sha256: helper.sha256, ...await writeAsset(directory, `ctl-${identity.bundle_id}-${target}.tar.gz`) }));
}

function uploadNames(calls: string[][]): string[] {
  return calls.filter((args) => args[1]?.startsWith("https://uploads.github.com/"))
    .map((args) => new URL(args[1]!).searchParams.get("name")!);
}

function assertPayloadsBeforeManifests(calls: string[][]): void {
  const names = uploadNames(calls);
  const first_manifest = names.findIndex((name) => name.endsWith(".json"));
  assert.ok(first_manifest >= 0);
  assert.ok(names.slice(first_manifest).every((name) => name.endsWith(".json")));
  assert.ok(names.slice(0, first_manifest).every((name) => !name.endsWith(".json")));
}

test("publishes verified components without desktop artifacts and preserves manual assets/notes", async (t) => {
  const root = await fixture(t);
  const directory = await componentSetFixture(root, identity);
  await cp(await windowsComponentSetFixture(root, identity), directory, { recursive: true });
  const api = github({ release: { ...published, draft: true }, assets: [
    { id: 1, name: "old-development.dmg", label: `${managedLabelPrefix}old-development.dmg`, digest: "old", size: 1 },
    { id: 2, name: "manual.pdf", label: null, digest: "manual", size: 1 },
  ] });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "components", api.gh);
  assert.equal(api.release()?.draft, false);
  assert.ok(api.release()?.body?.startsWith("Maintainer notes"));
  assert.equal(api.assets.length, 16);
  assert.ok(api.release()?.body?.includes("Windows x64"));
  assert.ok(api.assets.some((asset) => asset.name === "manual.pdf"));
  assert.ok(!api.assets.some((asset) => asset.name === "old-development.dmg"));
  const last_upload = api.calls.map((args) => args[1]!.startsWith("https://uploads.github.com")).lastIndexOf(true);
  const deletion = api.calls.findIndex((args) => args.includes("DELETE"));
  const publication = api.calls.findIndex((args) => args.includes("PATCH"));
  assert.ok(last_upload < deletion && deletion < publication);
  assertPayloadsBeforeManifests(api.calls);
  const uploads = uploadNames(api.calls);
  assert.ok(uploads.indexOf(updaterManifestName(windowsAgentTarget)) < uploads.indexOf(windowsAgentManifestName));
});

test("an empty published release receives components without canonical downloads", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  const api = github({ release: published });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "components", api.gh);
  assert.equal(api.release()?.draft, false);
  assert.equal(api.assets.length, 9);
  assert.ok(api.release()?.body?.startsWith("Maintainer notes"));
  assert.ok(api.release()?.body?.includes("verified Linux and macOS component bundles"));
  assert.ok(!api.calls.some((args) => args[0] === "release" || args.includes("DELETE")));
  assert.equal(uploadNames(api.calls).length, 9);
  assert.equal(api.patches.length, 1);
  assert.ok(!("draft" in api.patches[0]!));
  assertPayloadsBeforeManifests(api.calls);
});

test("a tag retry appends a Windows companion to a published Unix-only release", async (t) => {
  const root = await fixture(t), canonical = await componentSetFixture(root, identity), rebuilt = join(root, "rebuilt");
  await cp(canonical, rebuilt, { recursive: true });
  await cp(await windowsComponentSetFixture(root, identity), rebuilt, { recursive: true });
  const api = github({ release: published, assets: await existingComponents(canonical), canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, rebuilt, "components", api.gh);
  const uploads = uploadNames(api.calls);
  assert.equal(uploads.length, 6);
  assert.equal(uploads.at(-1), windowsAgentManifestName);
  assert.ok(uploads.includes(updaterManifestName(windowsAgentTarget)));
  assert.ok(api.release()?.body?.includes("Windows x64"));
  assert.ok(!api.calls.some((args) => args.includes("DELETE")));
});

test("Windows desktop append reuses the canonical x64 runtime updater and adds ARM64 independently", async (t) => {
  const canonical_root = await fixture(t);
  const canonical = await componentSetFixture(canonical_root, identity);
  await cp(await windowsComponentSetFixture(canonical_root, identity), canonical, { recursive: true });
  const original = await existingComponents(canonical);
  const incoming_root = await fixture(t);
  const directory = await componentSetFixture(incoming_root, identity);
  await cp(await windowsComponentSetFixture(incoming_root, identity), directory, { recursive: true });
  for (const target of windowsUpdaterTargets) await desktopAssets(directory, target);
  const updater = updaterBinaryName(identity, windowsAgentTarget);
  assert.notDeepEqual(await readFile(join(directory, updater)), await readFile(join(canonical, updater)));
  const api = github({ release: published, assets: original, canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);
  assert.deepEqual(await readFile(join(directory, updater)), await readFile(join(canonical, updater)));
  assert.deepEqual(api.assets.find((asset) => asset.name === updater), original.find((asset) => asset.name === updater));
  const uploads = uploadNames(api.calls);
  assert.equal(uploads.length, 9);
  assert.ok(!uploads.includes(updater));
  assert.ok(!uploads.includes(updaterManifestName(windowsAgentTarget)));
  assert.ok(uploads.includes(updaterManifestName(windowsUpdaterTargets[1])));
  assert.equal(new Set(api.assets.map((asset) => asset.name)).size, api.assets.length);
  assert.deepEqual((await validateReleaseBundle(buildIdentity, directory, { allow_partial: true })).desktop_targets, windowsUpdaterTargets);
  assertPayloadsBeforeManifests(api.calls);
});

test("a partial Linux desktop appends without macOS desktops or helpers", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  const components = await existingComponents(directory), target = "x86_64-unknown-linux-gnu";
  await desktopAssets(directory, target);
  const manual = { id: 42, name: `ctmux-${identity.bundle_id}-source.zip`, label: null, digest: "manual", size: 1 };
  const api = github({ release: published, assets: [...components, manual], canonical_directory: directory });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);
  assert.equal(api.assets.length, 17);
  assert.deepEqual(api.assets.find((asset) => asset.name === manual.name), manual);
  assert.ok(!api.calls.some((args) => args[0] === "release" && args.includes(manual.name)));
  assert.equal(uploadNames(api.calls).length, 7);
  assert.ok(api.release()?.body?.includes(`Desktop packages: \`${target}\`.`));
  assert.ok(!api.release()?.body?.includes("apple-darwin"));
  assert.ok(!api.release()?.body?.includes("ctld.app"));
  assert.ok(api.release()?.body?.startsWith("Maintainer notes"));
  assert.ok(!api.calls.some((args) => args.includes("DELETE")));
  assertPayloadsBeforeManifests(api.calls);
});

test("one signed helper appends without a desktop or CLI package", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  const components = await existingComponents(directory), target = "aarch64-apple-darwin";
  await helperAssets(directory, target);
  const api = github({ release: published, assets: components, canonical_directory: directory });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);
  assert.equal(api.assets.length, 12);
  assert.equal(uploadNames(api.calls).length, 3);
  assert.ok(api.release()?.body?.includes(`ctld.app bundles for: \`${target}\`.`));
  assert.ok(!api.release()?.body?.includes("Desktop packages:"));
  assert.ok(!api.release()?.body?.includes("ctl CLI archives"));
  assertPayloadsBeforeManifests(api.calls);
});

test("a later target append retains canonical target bytes and lists the target union", async (t) => {
  const canonical = await componentSetFixture(await fixture(t), identity);
  const old_target = "x86_64-unknown-linux-gnu", new_target = "aarch64-unknown-linux-gnu";
  await desktopAssets(canonical, old_target);
  const original = await existingReleaseAssets(canonical);
  const original_body = releaseNotes(await validateReleaseBundle(buildIdentity, canonical, { allow_partial: true }), "Maintainer notes");
  const directory = await componentSetFixture(await fixture(t), identity);
  await desktopAssets(directory, new_target);
  const api = github({ release: { ...published, body: original_body }, assets: original, canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);
  assert.equal(api.assets.length, 23);
  assert.equal(uploadNames(api.calls).length, 7);
  assert.ok(uploadNames(api.calls).every((name) => name.includes(new_target)));
  for (const asset of original) {
    assert.equal(await fileDigest(join(directory, asset.name)), asset.digest);
    assert.deepEqual(api.assets.find((entry) => entry.name === asset.name), asset);
  }
  const notes = api.release()?.body;
  assert.ok(notes?.startsWith("Maintainer notes"));
  assert.ok(notes?.includes(`Desktop packages: \`${old_target}\`, \`${new_target}\`.`));
  assert.ok(!api.calls.some((args) => args.includes("DELETE")));
  assertPayloadsBeforeManifests(api.calls);
});

test("a later desktop append restores the canonical Windows ZIP and complete updater group", async (t) => {
  const canonical = await componentSetFixture(await fixture(t), identity);
  const old_target = windowsUpdaterTargets[1], new_target = "x86_64-unknown-linux-gnu";
  await desktopAssets(canonical, old_target);
  const original = await existingReleaseAssets(canonical);
  const directory = await componentSetFixture(await fixture(t), identity);
  await desktopAssets(directory, new_target);
  const api = github({ release: published, assets: original, canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);
  for (const asset of original.filter((asset) => asset.name.includes(old_target))) {
    assert.deepEqual(await readFile(join(directory, asset.name)), await readFile(join(canonical, asset.name)));
    assert.deepEqual(api.assets.find((entry) => entry.name === asset.name), asset);
  }
  assert.equal(original.filter((asset) => asset.name.includes(old_target)).length, 6);
  assert.equal(uploadNames(api.calls).length, 7);
  assert.ok(uploadNames(api.calls).every((name) => name.includes(new_target)));
  assert.ok(api.release()?.body?.includes(old_target));
  assert.ok(!api.calls.some((args) => args.includes("DELETE")));
});

test("a rebuilt target reuses canonical bytes while a new target appends", async (t) => {
  const canonical = await componentSetFixture(await fixture(t), identity), old_target = "x86_64-apple-darwin";
  await desktopAssets(canonical, old_target);
  const directory = join(dirname(canonical), "rebuilt-with-new-target");
  await cp(canonical, directory, { recursive: true });
  const changed = `ctmux-${identity.bundle_id}-${old_target}.dmg`;
  const rebuilt = await writeAsset(directory, changed, "fresh signed package with different valid bytes");
  const manifest_name = `desktop-${old_target}.json`, manifest_path = join(directory, manifest_name);
  const manifest = JSON.parse(await readFile(manifest_path, "utf8"));
  manifest.assets.find((asset: { name: string }) => asset.name === changed).sha256 = rebuilt.sha256;
  await writeFile(manifest_path, JSON.stringify(manifest));
  const new_target = "aarch64-unknown-linux-gnu";
  await desktopAssets(directory, new_target);
  await validateReleaseBundle(buildIdentity, directory, { allow_partial: true });
  assert.notEqual(await fileDigest(join(directory, changed)), await fileDigest(join(canonical, changed)));
  const api = github({ release: published, assets: await existingReleaseAssets(canonical), canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);
  for (const name of [changed, `${changed}.sha256`, manifest_name]) {
    assert.deepEqual(await readFile(join(directory, name)), await readFile(join(canonical, name)));
  }
  assert.equal(uploadNames(api.calls).length, 7);
  assert.ok(uploadNames(api.calls).every((name) => name.includes(new_target)));
  assert.ok(api.release()?.body?.includes(old_target));
  assert.ok(api.release()?.body?.includes(new_target));
  assert.ok(!api.calls.some((args) => args.includes("DELETE")));
});

test("an unsigned desktop preserves Windows assets and a later signed build appends beside it", async (t) => {
  const canonical_root = await fixture(t);
  const canonical = await componentSetFixture(canonical_root, identity);
  await cp(await windowsComponentSetFixture(canonical_root, identity), canonical, { recursive: true });
  for (const target of windowsUpdaterTargets) await desktopAssets(canonical, target);
  const target = "aarch64-apple-darwin";
  await desktopAssets(canonical, "x86_64-unknown-linux-gnu");
  const fallback = await componentSetFixture(await fixture(t), identity);
  await desktopAssets(fallback, target, true);

  const fallback_api = github({ release: published, assets: await existingReleaseAssets(canonical), canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, fallback, "desktop", fallback_api.gh);
  const fallback_uploads = uploadNames(fallback_api.calls);
  assert.equal(fallback_uploads.length, 5);
  assert.ok(fallback_uploads.every((name) => name.includes(`${target}-unsigned`)));
  assert.equal(fallback_uploads.at(-1), `desktop-${target}-unsigned.json`);
  assert.match(fallback_api.release()?.body ?? "", /[Uu]nsigned/);

  const directory = await componentSetFixture(await fixture(t), identity);
  await desktopAssets(directory, target);
  const original = fallback_api.assets;
  const api = github({ release: fallback_api.release()!, assets: original, canonical_directory: fallback });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);

  const uploads = uploadNames(api.calls);
  assert.equal(uploads.length, 5);
  assert.ok(uploads.every((name) => name.includes(target) && !name.includes("-unsigned")));
  assert.equal(uploads.at(-1), `desktop-${target}.json`);
  for (const asset of original) {
    assert.equal(await fileDigest(join(directory, asset.name)), asset.digest);
    assert.deepEqual(api.assets.find((entry) => entry.name === asset.name), asset);
  }
  assert.ok(api.release()?.body?.includes(`\`${target}\``));
  assert.match(api.release()?.body ?? "", /[Uu]nsigned/);
  assert.ok(!api.calls.some((args) => args.includes("DELETE")));
});

test("an interrupted desktop upload retries only missing files before its manifest", async (t) => {
  const canonical = await componentSetFixture(await fixture(t), identity), components = await existingComponents(canonical);
  const target = "x86_64-unknown-linux-gnu";
  await desktopAssets(canonical, target);
  const interrupted = github({ release: published, assets: components, canonical_directory: canonical, fail_after_upload: 2 });
  await assert.rejects(publishComponentRelease("tokn-ai/ctl", identity, canonical, "desktop", interrupted.gh), /upload failed/);
  assert.equal(interrupted.assets.length, 11);
  assert.ok(!interrupted.assets.some((asset) => asset.name === `desktop-${target}.json`));
  assert.ok(!interrupted.calls.some((args) => args.includes("PATCH") || args.includes("DELETE")));
  const already_uploaded = interrupted.assets.filter((asset) => !components.some((component) => component.name === asset.name));
  const directory = join(dirname(canonical), "interrupted-retry");
  await cp(canonical, directory, { recursive: true });
  const retry = github({ release: interrupted.release()!, assets: interrupted.assets, canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", retry.gh);
  assert.equal(retry.assets.length, 16);
  assert.equal(uploadNames(retry.calls).length, 5);
  assert.ok(uploadNames(retry.calls).every((name) => !already_uploaded.some((asset) => asset.name === name)));
  for (const asset of already_uploaded) assert.deepEqual(retry.assets.find((entry) => entry.name === asset.name), asset);
  assert.ok(retry.release()?.body?.includes(`Desktop packages: \`${target}\`.`));
  assert.ok(!retry.calls.some((args) => args.includes("DELETE")));
  assertPayloadsBeforeManifests(retry.calls);
});

test("a Linux target appends while an interrupted macOS helper has no published manifest", async (t) => {
  const canonical = await componentSetFixture(await fixture(t), identity);
  const mac = "aarch64-apple-darwin";
  const linux = "x86_64-unknown-linux-gnu";
  const components = await existingComponents(canonical);
  await helperAssets(canonical, mac);
  const partial = github({ release: published, assets: components, canonical_directory: canonical, fail_after_upload: 2 });
  await assert.rejects(publishComponentRelease("tokn-ai/ctl", identity, canonical, "desktop", partial.gh), /upload failed/);
  const orphans = partial.assets.filter((asset) => asset.name.startsWith("ctld-"));
  assert.equal(orphans.length, 2);
  assert.ok(orphans.every((asset) => !asset.name.endsWith(".json")));

  const incoming = await componentSetFixture(await fixture(t), identity);
  await desktopAssets(incoming, linux);
  const api = github({ release: partial.release()!, assets: partial.assets, canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, incoming, "desktop", api.gh);
  for (const orphan of orphans) {
    assert.deepEqual(api.assets.find((asset) => asset.name === orphan.name), orphan);
    assert.ok(!api.calls.some((args) => args[0] === "release" && args.includes(orphan.name)));
  }
  assert.ok(!api.calls.some((args) => args.includes("DELETE")));
  assert.equal(uploadNames(api.calls).length, 7);
  assert.ok(uploadNames(api.calls).every((name) => name.includes(linux)));
  assert.ok(api.release()?.body?.includes(`Desktop packages: \`${linux}\`.`));
  assert.ok(!api.release()?.body?.includes(mac));
});

test("published partial target digest errors and manual collisions fail before mutations", async (t) => {
  for (const manual of [false, true]) {
    await t.test(manual ? "manual asset collision" : "existing digest mismatch", async (t) => {
      const canonical = await componentSetFixture(await fixture(t), identity), target = "x86_64-unknown-linux-gnu";
      await desktopAssets(canonical, target);
      const changed = `ctmux-${identity.bundle_id}-${target}.deb`;
      const assets = (await existingReleaseAssets(canonical)).map((asset) => asset.name === changed
        ? { ...asset, ...(manual ? { label: null } : { digest: "sha256:wrong" }) } : asset);
      const directory = await componentSetFixture(await fixture(t), identity);
      await desktopAssets(directory, "aarch64-unknown-linux-gnu");
      const api = github({ release: published, assets, canonical_directory: canonical });
      await assert.rejects(publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh), /immutable digest|manually managed/);
      assert.ok(!api.calls.some((args) => args.includes("POST") || args.includes("PATCH") || args.includes("DELETE")));
    });
  }
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
  await cp(await windowsComponentSetFixture(root, identity), canonical, { recursive: true });
  await cp(canonical, rebuilt, { recursive: true });
  const changed_assets: string[] = [];
  for (const [filename, target_triple] of [["bundle-set.json", "x86_64-apple-darwin"], [windowsAgentManifestName, windowsAgentTarget]]) {
    const manifest = JSON.parse(await readFile(join(rebuilt, filename), "utf8"));
    const target = manifest.targets[target_triple];
    const archive = await readFile(join(rebuilt, target.archive));
    archive.writeUInt32LE(12345678, 4);
    target.sha256 = createHash("sha256").update(archive).digest("hex");
    await writeFile(join(rebuilt, target.archive), archive);
    await writeFile(join(rebuilt, `${target.archive}.sha256`), `${target.sha256}  ${target.archive}\n`);
    await writeFile(join(rebuilt, filename), JSON.stringify(manifest));
    changed_assets.push(target.archive, `${target.archive}.sha256`, filename);
  }
  await validatedComponentSet(rebuilt, identity);
  const api = github({ release: published, assets: await existingComponents(canonical), canonical_directory: canonical });
  await publishComponentRelease("tokn-ai/ctl", identity, rebuilt, "components", api.gh);
  for (const name of changed_assets) assert.deepEqual(await readFile(join(rebuilt, name)), await readFile(join(canonical, name)));
  assert.ok(!api.calls.some((args) => args.includes("POST") || args.includes("PATCH") || args.includes("DELETE")));
});

test("desktop packages append to an already-published matching component release", async (t) => {
  const directory = await componentSetFixture(await fixture(t), identity);
  const api = github({ release: published, assets: await existingComponents(directory), canonical_directory: directory });
  for (const target of ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-apple-darwin", "aarch64-apple-darwin", ...windowsUpdaterTargets]) {
    await desktopAssets(directory, target);
    if (target.endsWith("apple-darwin")) await helperAssets(directory, target, true);
  }
  await publishComponentRelease("tokn-ai/ctl", identity, directory, "desktop", api.gh);
  assert.equal(api.release()?.draft, false);
  assert.ok(api.release()?.body?.startsWith("Maintainer notes"));
  assert.ok(api.assets.some((asset) => asset.name === "desktop-aarch64-apple-darwin.json"));
  assert.ok(api.assets.some((asset) => asset.name === "ctl-cli-aarch64-apple-darwin.json"));
  for (const target of windowsUpdaterTargets) {
    assert.ok(api.assets.some((asset) => asset.name === `desktop-${target}.json`));
    assert.ok(api.assets.some((asset) => asset.name === updaterBinaryName(identity, target)));
  }
  const uploads = uploadNames(api.calls);
  for (const target of ["x86_64-apple-darwin", "aarch64-apple-darwin"]) {
    const manifests = [`ctld-${target}.json`, `ctl-cli-${target}.json`];
    assert.deepEqual(uploads.filter((name) => manifests.includes(name)), manifests,
      "the matching helper manifest must be available before its CLI is advertised");
  }
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
