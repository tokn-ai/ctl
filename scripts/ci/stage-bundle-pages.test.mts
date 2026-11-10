import assert from "node:assert/strict";
import { lstat, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { bundleRoot, validatedComponentSet } from "./bundle-publication.mts";
import { componentSetFixture, publicationFixture } from "./bundle-publication.fixtures.mts";
import { compareVersions, devRetention, parseBundleCatalog, stageBundlePages } from "./stage-bundle-pages.mts";

async function root(t: TestContext): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "ctl-pages-test-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}

test("stages verified archives and aliases Nightly to Dev without copying a bundle set", async (t) => {
  const directory = await root(t);
  const stable = publicationFixture(1, "stable"), dev = publicationFixture(2);
  const previous = join(directory, "previous"), next = join(directory, "next");
  assert.equal(await stageBundlePages(stable, await componentSetFixture(directory, stable), null, previous), true);
  await assert.rejects(readFile(join(previous, "bundles/channels/nightly.json")), { code: "ENOENT" });
  assert.equal(await stageBundlePages(dev, await componentSetFixture(directory, dev), previous, next), true);
  for (const publication of [stable, dev]) {
    const pointer = JSON.parse(await readFile(join(next, "bundles/channels", `${publication.channel}.json`), "utf8"));
    assert.deepEqual(pointer, { schema_version: 1, channel: publication.channel, bundle_set_url: `${bundleRoot}sets/${publication.publication_id}/bundle-set.json` });
    const { asset_names } = await validatedComponentSet(join(next, "bundles/sets", publication.publication_id), publication);
    assert.equal(asset_names.length, 9);
  }
  assert.deepEqual(JSON.parse(await readFile(join(next, "bundles/channels/nightly.json"), "utf8")), {
    schema_version: 1, channel: "nightly", bundle_set_url: `${bundleRoot}sets/${dev.publication_id}/bundle-set.json`,
  });
  assert.deepEqual((await readdir(join(next, "bundles/sets"))).sort(), [stable.publication_id, dev.publication_id].sort());
  const catalog = parseBundleCatalog(await readFile(join(next, "bundles/catalog.json")));
  assert.equal(catalog.channels.nightly, catalog.channels.dev);
  assert.equal(catalog.generation, 2);
});

test("backfills Nightly from a legacy Dev head on release publication and rejects a divergent alias", async (t) => {
  const directory = await root(t);
  const dev = publicationFixture(1), stable = publicationFixture(2, "stable");
  const previous = join(directory, "previous"), next = join(directory, "next");
  await stageBundlePages(dev, await componentSetFixture(directory, dev), null, previous);
  const catalog_path = join(previous, "bundles/catalog.json");
  const legacy = parseBundleCatalog(await readFile(catalog_path));
  delete legacy.channels.nightly;
  await writeFile(catalog_path, JSON.stringify(legacy));
  await rm(join(previous, "bundles/channels/nightly.json"));

  await stageBundlePages(stable, await componentSetFixture(directory, stable), previous, next);
  const catalog = parseBundleCatalog(await readFile(join(next, "bundles/catalog.json")));
  assert.equal(catalog.channels.nightly, dev.publication_id);
  assert.equal(catalog.channels.stable, stable.publication_id);
  catalog.channels.nightly = stable.publication_id;
  assert.throws(() => parseBundleCatalog(Buffer.from(JSON.stringify(catalog))), /same publication as Dev/);
});

test("old main runs and lower release versions cannot roll channels back", async (t) => {
  const directory = await root(t);
  const newer = publicationFixture(3), older = publicationFixture(2);
  const previous = join(directory, "previous");
  await stageBundlePages(newer, await componentSetFixture(directory, newer), null, previous);
  const output = join(directory, "ignored");
  assert.equal(await stageBundlePages(older, await componentSetFixture(directory, older), previous, output), false);
  await assert.rejects(readFile(join(output, "bundles/catalog.json")), { code: "ENOENT" });
  const stable = publicationFixture(4, "stable", "2.0.0"), lower = publicationFixture(5, "stable", "1.9.9");
  const stable_site = join(directory, "stable");
  await stageBundlePages(stable, await componentSetFixture(directory, stable), previous, stable_site);
  assert.equal(await stageBundlePages(lower, await componentSetFixture(directory, lower), stable_site, output), true);
  assert.equal(parseBundleCatalog(await readFile(join(output, "bundles/catalog.json"))).channels.stable, stable.publication_id);
});

test("a versioned prerelease archive is mirrored without moving the stable pointer", async (t) => {
  const directory = await root(t);
  const stable = publicationFixture(1, "stable", "1.0.0"), beta = publicationFixture(2, "stable", "2.0.0-beta.1");
  const previous = join(directory, "stable"), output = join(directory, "beta");
  await stageBundlePages(stable, await componentSetFixture(directory, stable), null, previous);
  const input = await componentSetFixture(directory, beta);
  await stageBundlePages(beta, input, previous, output);
  const catalog = parseBundleCatalog(await readFile(join(output, "bundles/catalog.json")));
  assert.equal(catalog.channels.stable, stable.publication_id);
  await validatedComponentSet(join(output, "bundles/sets", beta.publication_id), beta);
  const only_beta = join(directory, "only-beta");
  await stageBundlePages(beta, input, null, only_beta);
  assert.equal(parseBundleCatalog(await readFile(join(only_beta, "bundles/catalog.json"))).channels.stable, undefined);
});

test("retains five dev generations and two stable versions without dropping current pointers", async (t) => {
  const directory = await root(t);
  let previous: string | null = null;
  for (let run = 1; run <= 9; run += 1) {
    const entry = run <= 3 ? publicationFixture(run, "stable", `0.${run}.0`) : publicationFixture(run);
    const next = join(directory, `site-${run}`);
    await stageBundlePages(entry, await componentSetFixture(directory, entry), previous, next);
    previous = next;
  }
  const catalog = parseBundleCatalog(await readFile(join(previous!, "bundles/catalog.json")));
  assert.equal(catalog.publications.filter((entry) => entry.channel === "dev").length, devRetention);
  assert.equal(catalog.publications.filter((entry) => entry.channel === "stable").length, 2);
  assert.equal((await readdir(join(previous!, "bundles/sets"))).length, 7);
  for (const id of Object.values(catalog.channels)) assert.ok(catalog.publications.some((entry) => entry.publication_id === id));
});

async function treeBytes(path: string): Promise<number> {
  const metadata = await lstat(path);
  if (metadata.isFile()) return metadata.size;
  let bytes = 0;
  for (const name of await readdir(path)) bytes += await treeBytes(join(path, name));
  return bytes;
}

test("byte pressure evicts oldest unselected Dev generations while pinning channel heads and the newest other tag", async (t) => {
  const directory = await root(t);
  const stable = publicationFixture(1, "stable", "1.0.0");
  const tag = publicationFixture(2, "stable", "2.0.0-beta.1");
  const dev = [3, 4, 5].map((run) => publicationFixture(run));
  let previous: string | null = null;
  for (const entry of [stable, tag, ...dev.slice(0, 2)]) {
    const next = join(directory, `site-${entry.run_id}`);
    await stageBundlePages(entry, await componentSetFixture(directory, entry), previous, next);
    previous = next;
  }
  const before = await readFile(join(previous!, "bundles/catalog.json"));
  const incoming = dev[2];
  const input = await componentSetFixture(directory, incoming);
  const complete = join(directory, "complete");
  await stageBundlePages(incoming, input, previous, complete);
  const full_size = await treeBytes(complete);
  const oldest_size = await treeBytes(join(complete, "bundles/sets", dev[0].publication_id));
  const budget = full_size - oldest_size;
  const output = join(directory, "bounded");
  await stageBundlePages(incoming, input, previous, output, budget);
  assert.ok(await treeBytes(output) <= budget);
  const catalog = parseBundleCatalog(await readFile(join(output, "bundles/catalog.json")));
  assert.deepEqual(catalog.publications.map((entry) => entry.publication_id), [incoming, dev[1], stable, tag].map((entry) => entry.publication_id));
  assert.equal(catalog.channels.dev, incoming.publication_id);
  assert.equal(catalog.channels.nightly, incoming.publication_id);
  assert.equal(catalog.channels.stable, stable.publication_id);
  await assert.rejects(lstat(join(output, "bundles/sets", dev[0].publication_id)), { code: "ENOENT" });
  for (const entry of catalog.publications) await validatedComponentSet(join(output, "bundles/sets", entry.publication_id), entry);
  assert.deepEqual(await readFile(join(previous!, "bundles/catalog.json")), before);
  await validatedComponentSet(join(previous!, "bundles/sets", dev[0].publication_id), dev[0]);
  const impossible = join(directory, "impossible");
  await assert.rejects(stageBundlePages(incoming, input, previous, impossible, 1), /Pinned bundle publications exceed/);
  await assert.rejects(lstat(impossible), { code: "ENOENT" });
  assert.deepEqual(await readFile(join(previous!, "bundles/catalog.json")), before);
});

test("failed validation or capacity checks leave the previous deployment intact and never expose a pointer", async (t) => {
  const directory = await root(t);
  const old = publicationFixture(1), incoming = publicationFixture(2);
  const previous = join(directory, "previous"), output = join(directory, "failed");
  await stageBundlePages(old, await componentSetFixture(directory, old), null, previous);
  const before = await readFile(join(previous, "bundles/catalog.json"));
  const input = await componentSetFixture(directory, incoming);
  await assert.rejects(stageBundlePages(incoming, input, previous, output, 1), /safety limit/);
  await assert.rejects(readFile(join(output, "bundles/catalog.json")), { code: "ENOENT" });
  await writeFile(join(input, `ctl-agent-bundle-${incoming.bundle_id}-x86_64-apple-darwin.tar.gz`), "corrupt");
  await assert.rejects(stageBundlePages(incoming, input, previous, output), /checksum/);
  assert.deepEqual(await readFile(join(previous, "bundles/catalog.json")), before);
});

test("unmanaged content, mixed pointers and moved release tags fail closed", async (t) => {
  const directory = await root(t);
  const old = publicationFixture(1, "stable"), next = publicationFixture(2, "stable");
  const previous = join(directory, "previous"), output = join(directory, "failed");
  await stageBundlePages(old, await componentSetFixture(directory, old), null, previous);
  const input = await componentSetFixture(directory, next);
  await assert.rejects(stageBundlePages(next, input, previous, output), /cannot change source revision/);
  const dev = publicationFixture(3);
  const dev_input = await componentSetFixture(directory, dev);
  await writeFile(join(previous, "manual.html"), "preserve me");
  await assert.rejects(stageBundlePages(dev, dev_input, previous, output), /non-bundle Pages content/);
  await rm(join(previous, "manual.html"));
  await writeFile(join(previous, "bundles/channels/stable.json"), JSON.stringify({ schema_version: 1, channel: "stable", bundle_set_url: "https://example.com/wrong" }));
  await assert.rejects(stageBundlePages(dev, dev_input, previous, output), /disagree/);
});

test("release ordering follows semver rather than lexical or workflow run ordering", () => {
  for (const [low, high] of [["0.9.0", "0.10.0"], ["1.0.0-rc.2", "1.0.0-rc.10"], ["1.0.0-rc.10", "1.0.0"], ["1.0.0-1", "1.0.0-alpha"]]) assert.equal(compareVersions(low, high), -1);
  assert.equal(compareVersions("1.0.0+first", "1.0.0+second"), 0);
  assert.throws(() => compareVersions("1.0.0-01", "1.0.0"), /Invalid release version/);
});
