import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { agentTargets } from "../shared/agent-bundle.mts";
import { parseUpdaterManifest, updaterManifestName, verifyUpdaterTarget } from "../shared/updater-bundle.mts";
import { agentIdentity, componentMapFixture } from "./agent-bundle.fixtures.mts";
import { packageUpdater } from "./package-updater.mts";
import { componentSetFixture, publicationFixture } from "./bundle-publication.fixtures.mts";
import { validatedComponentSet } from "./bundle-publication.mts";
import { assembleAgentBundleSet } from "./assemble-agent-bundle-set.mts";
import { stageBundlePages } from "./stage-bundle-pages.mts";
import { restoreBundlePages } from "./restore-bundle-pages.mts";
import type { GhRunner } from "./update-draft-release.mts";

async function directory(t: TestContext): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "ctlup-package-test-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}

test("standalone updater metadata binds the exact source, target and executable bytes", async (t) => {
  const root = await directory(t);
  const binary_directory = join(root, "release");
  await mkdir(binary_directory);
  await writeFile(join(binary_directory, "ctlup"), "updater payload");
  await chmod(join(binary_directory, "ctlup"), 0o700);
  const target = agentTargets[0];
  const options = { ...agentIdentity, target, binary_directory, output_directory: join(root, "output"), strip: false };
  const probe = async () => ({ stdout: JSON.stringify(componentMapFixture().ctl), stderr: "" });
  const manifest = await packageUpdater(options, probe);
  assert.equal(manifest.sha256, createHash("sha256").update("updater payload").digest("hex"));
  assert.equal((await verifyUpdaterTarget(options.output_directory, agentIdentity, target, true)).length, 3);
  for (const change of [{ target_triple: agentTargets[1] }, { git_revision: "b".repeat(40) }, { binary: "../ctlup" }, { size: 0 }, { extra: true }]) {
    assert.throws(() => parseUpdaterManifest(Buffer.from(JSON.stringify({ ...manifest, ...change })), agentIdentity, target));
  }
  await writeFile(join(options.output_directory, manifest.binary), "changed updater");
  await assert.rejects(verifyUpdaterTarget(options.output_directory, agentIdentity, target, true), /checksum/);
  const dirty = componentMapFixture().ctl;
  dirty.build.dirty = true;
  await assert.rejects(packageUpdater(options, async () => ({ stdout: JSON.stringify(dirty), stderr: "" })), /clean publication/);
});

test("complete updater sidecars survive assembly and Pages staging outside runtime manifests", async (t) => {
  const root = await directory(t);
  const publication = publicationFixture(1);
  const incoming = await componentSetFixture(root, publication);
  const binary_directory = join(root, "release");
  await mkdir(binary_directory);
  await writeFile(join(binary_directory, "ctlup"), "updater");
  await chmod(join(binary_directory, "ctlup"), 0o700);
  for (const target of agentTargets) {
    await packageUpdater({ ...publication, target, binary_directory, output_directory: incoming, strip: false }, async () => ({
      stdout: JSON.stringify(componentMapFixture(publication).ctl), stderr: "",
    }));
  }
  const verified = await validatedComponentSet(incoming, publication);
  assert.equal(verified.asset_names.length, 21);
  assert.equal(verified.manifest.schema_version, 3);
  assert.equal(Object.keys(verified.manifest.targets[agentTargets[0]].components!).length, 6);
  const assembled = join(root, "assembled");
  const { app_version, bundle_id, git_revision } = publication;
  await assembleAgentBundleSet({ app_version, bundle_id, git_revision }, incoming, assembled);
  assert.equal((await readdir(assembled)).length, 21);
  const site = join(root, "site");
  await stageBundlePages(publication, assembled, null, site);
  const published = join(site, "bundles/sets", publication.publication_id);
  assert.deepEqual((await readdir(published)).sort(), (await readdir(assembled)).sort());
  assert.deepEqual(await readFile(join(published, "bundle-set.json")), await readFile(join(assembled, "bundle-set.json")));
  const gh: GhRunner = async (args) => {
    if (args[1]!.endsWith("/pages")) return JSON.stringify({ build_type: "workflow", html_url: "https://tokn-ai.dev/ctl/" });
    if (args[1]!.includes("/actions/artifacts")) return "[{\"artifacts\":[]}]";
    if (args[1]!.includes("/deployments?")) return "[[{\"id\":1}]]";
    if (args[1]!.includes("/statuses?")) return "[[{\"state\":\"success\"}]]";
    throw new Error(`Unexpected request: ${args.join(" ")}`);
  };
  const fetcher: typeof fetch = async (input) => {
    const url = new URL(String(input));
    try { return new Response(await readFile(join(site, url.pathname.slice("/ctl/".length)))); }
    catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return new Response("missing", { status: 404 }); throw error; }
  };
  const restored = join(root, "restored");
  assert.equal(await restoreBundlePages("tokn-ai/ctl", restored, gh, fetcher), true);
  assert.deepEqual((await readdir(join(restored, "bundles/sets", publication.publication_id))).sort(), (await readdir(assembled)).sort());
  await rm(join(incoming, updaterManifestName(agentTargets[0])));
  await assert.rejects(validatedComponentSet(incoming, publication), /incomplete ctlup/);
});
