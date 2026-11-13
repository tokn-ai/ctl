import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { buildInfoName, parseBuildInfo } from "../shared/build-info.mts";
import { componentSetFixture, publicationFixture } from "./bundle-publication.fixtures.mts";
import { validatedComponentSet } from "./bundle-publication.mts";
import { stageBundlePages } from "./stage-bundle-pages.mts";

test("assembly stamps and publication preserves a source-bound build time outside component hashes", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ctl-build-info-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const identity = publicationFixture(1);
  const input = await componentSetFixture(root, identity, true);
  const metadata = await readFile(join(input, buildInfoName));
  const info = parseBuildInfo(metadata, identity);
  assert.ok(Date.now() - Date.parse(info.built_at) < 60_000);
  const { asset_names } = await validatedComponentSet(input, identity);
  assert.ok(asset_names.includes(buildInfoName));
  const site = join(root, "site");
  await stageBundlePages(identity, input, null, site);
  assert.deepEqual(await readFile(join(site, "bundles/sets", identity.publication_id, buildInfoName)), metadata);
  for (const [field, replacement] of [
    ["built_at", "2026-02-30T12:00:00Z"],
    ["built_at", "2026-11-13T12:00:00+01:00"],
    ["git_revision", "b".repeat(40)],
    ["schema_version", 2],
  ] as const) {
    const bad = { ...info, [field]: replacement };
    assert.throws(() => parseBuildInfo(Buffer.from(JSON.stringify(bad)), identity));
  }
  await writeFile(join(input, buildInfoName), JSON.stringify({ ...info, built_at: "invalid" }));
  await assert.rejects(validatedComponentSet(input, identity), /Build metadata/);
});

test("PR artifacts retain the strict legacy file layout", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ctl-pr-build-info-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const publication = publicationFixture(1);
  const bundle_id = `${publication.app_version}-dev.${publication.git_revision.slice(0, 12)}`;
  const input = await componentSetFixture(root, { ...publication, bundle_id }, true);
  await assert.rejects(readFile(join(input, buildInfoName)), { code: "ENOENT" });
});
