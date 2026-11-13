import assert from "node:assert/strict";
import { cp, lstat, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { windowsAgentManifestName } from "../shared/agent-bundle.mts";
import { bundleRoot, validatedComponentSet } from "./bundle-publication.mts";
import { componentSetFixture, legacyNightlyFixture, legacyPagesCatalog, publicationFixture, windowsComponentSetFixture } from "./bundle-publication.fixtures.mts";
import { compareVersions, nightlyRetention, parseBundleCatalog, stageBundlePages } from "./stage-bundle-pages.mts";

async function root(t: TestContext): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "ctl-pages-test-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}

test("stages verified dated Nightly archives and Stable archives", async (t) => {
  const directory = await root(t);
  const stable = publicationFixture(1, "stable"), nightly = publicationFixture(2);
  const previous = join(directory, "previous"), next = join(directory, "next");
  assert.equal(await stageBundlePages(stable, await componentSetFixture(directory, stable), null, previous), true);
  await assert.rejects(readFile(join(previous, "bundles/channels/nightly.json")), { code: "ENOENT" });
  assert.equal(await stageBundlePages(nightly, await componentSetFixture(directory, nightly), previous, next), true);
  for (const publication of [stable, nightly]) {
    const pointer = JSON.parse(await readFile(join(next, "bundles/channels", `${publication.channel}.json`), "utf8"));
    assert.deepEqual(pointer, { schema_version: 1, channel: publication.channel, bundle_set_url: `${bundleRoot}sets/${publication.publication_id}/bundle-set.json` });
    const { asset_names } = await validatedComponentSet(join(next, "bundles/sets", publication.publication_id), publication);
    assert.equal(asset_names.length, 9);
  }
  await assert.rejects(readFile(join(next, "bundles/channels/dev.json")), { code: "ENOENT" });
  assert.deepEqual((await readdir(join(next, "bundles/sets"))).sort(), [stable.publication_id, nightly.publication_id].sort());
  const catalog = parseBundleCatalog(await readFile(join(next, "bundles/catalog.json")));
  assert.equal(catalog.channels.nightly, nightly.publication_id);
  assert.equal(catalog.channels.dev, undefined);
  assert.equal(catalog.publications[0].channel, "nightly");
  assert.equal(catalog.generation, 2);
  const index = await readFile(join(next, "index.html"), "utf8");
  assert.match(index, /channels\/nightly\.json/);
  assert.match(index, /channels\/stable\.json/);
  assert.doesNotMatch(index, /channels\/dev\.json/);
});

test("a Stable tag retry publishes its Windows companion alongside the Unix set", async (t) => {
  const directory = await root(t);
  const first = publicationFixture(1, "stable");
  const retry = { ...first, run_attempt: 2, publication_id: `${first.bundle_id}-${first.run_id}-2` };
  const previous = join(directory, "previous"), next = join(directory, "next");
  await stageBundlePages(first, await componentSetFixture(directory, first), null, previous);
  const incoming = await componentSetFixture(directory, retry);
  await cp(await windowsComponentSetFixture(directory, retry), incoming, { recursive: true });
  assert.equal(await stageBundlePages(retry, incoming, previous, next), true);
  const catalog = parseBundleCatalog(await readFile(join(next, "bundles/catalog.json")));
  assert.equal(catalog.channels.stable, retry.publication_id);
  await readFile(join(next, "bundles/sets", retry.publication_id, windowsAgentManifestName));
  await assert.rejects(readFile(join(next, "bundles/sets", first.publication_id, windowsAgentManifestName)), { code: "ENOENT" });
});

test("migrates a legacy Dev head to Nightly and removes its pointer without changing the previous site", async (t) => {
  const directory = await root(t);
  const legacy = legacyNightlyFixture(2), stable = publicationFixture(3, "stable");
  const previous = join(directory, "previous"), next = join(directory, "next");
  await stageBundlePages(legacy, await componentSetFixture(directory, legacy), null, previous);
  await legacyPagesCatalog(previous, false);

  const stale = publicationFixture(1);
  assert.equal(await stageBundlePages(stale, await componentSetFixture(directory, stale), previous, next), false);

  await stageBundlePages(stable, await componentSetFixture(directory, stable), previous, next);
  const catalog = parseBundleCatalog(await readFile(join(next, "bundles/catalog.json")));
  assert.equal(catalog.channels.nightly, legacy.publication_id);
  assert.equal(catalog.channels.stable, stable.publication_id);
  assert.equal(catalog.channels.dev, undefined);
  await assert.rejects(readFile(join(next, "bundles/channels/dev.json")), { code: "ENOENT" });
  assert.match(await readFile(join(previous, "bundles/channels/dev.json"), "utf8"), /"channel":"dev"/);
  catalog.channels.dev = stable.publication_id;
  assert.throws(() => parseBundleCatalog(Buffer.from(JSON.stringify(catalog))), /Dev pointer must match Nightly/);

  const nightly = publicationFixture(4), advanced = join(directory, "advanced");
  await stageBundlePages(nightly, await componentSetFixture(directory, nightly), next, advanced);
  const migrated = parseBundleCatalog(await readFile(join(advanced, "bundles/catalog.json")));
  assert.deepEqual(migrated.channels, { stable: stable.publication_id, nightly: nightly.publication_id });
  assert.deepEqual(migrated.publications, [nightly, legacy, stable]);
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

test("retains five Nightly generations and two stable versions without dropping current pointers", async (t) => {
  const directory = await root(t);
  let previous: string | null = null;
  for (let run = 1; run <= 9; run += 1) {
    const entry = run <= 3 ? publicationFixture(run, "stable", `0.${run}.0`) : publicationFixture(run);
    const next = join(directory, `site-${run}`);
    await stageBundlePages(entry, await componentSetFixture(directory, entry), previous, next);
    previous = next;
  }
  const catalog = parseBundleCatalog(await readFile(join(previous!, "bundles/catalog.json")));
  assert.equal(catalog.publications.filter((entry) => entry.channel === "nightly").length, nightlyRetention);
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

test("byte pressure evicts oldest unselected Nightly generations while pinning channel heads and the newest other tag", async (t) => {
  const directory = await root(t);
  const stable = publicationFixture(1, "stable", "1.0.0");
  const tag = publicationFixture(2, "stable", "2.0.0-beta.1");
  const nightlies = [3, 4, 5].map((run) => publicationFixture(run));
  let previous: string | null = null;
  for (const entry of [stable, tag, ...nightlies.slice(0, 2)]) {
    const next = join(directory, `site-${entry.run_id}`);
    await stageBundlePages(entry, await componentSetFixture(directory, entry), previous, next);
    previous = next;
  }
  const before = await readFile(join(previous!, "bundles/catalog.json"));
  const incoming = nightlies[2];
  const input = await componentSetFixture(directory, incoming);
  const complete = join(directory, "complete");
  await stageBundlePages(incoming, input, previous, complete);
  const full_size = await treeBytes(complete);
  const oldest_size = await treeBytes(join(complete, "bundles/sets", nightlies[0].publication_id));
  const budget = full_size - oldest_size;
  const output = join(directory, "bounded");
  await stageBundlePages(incoming, input, previous, output, budget);
  assert.ok(await treeBytes(output) <= budget);
  const catalog = parseBundleCatalog(await readFile(join(output, "bundles/catalog.json")));
  assert.deepEqual(catalog.publications.map((entry) => entry.publication_id), [incoming, nightlies[1], stable, tag].map((entry) => entry.publication_id));
  assert.equal(catalog.channels.dev, undefined);
  assert.equal(catalog.channels.nightly, incoming.publication_id);
  assert.equal(catalog.channels.stable, stable.publication_id);
  await assert.rejects(lstat(join(output, "bundles/sets", nightlies[0].publication_id)), { code: "ENOENT" });
  for (const entry of catalog.publications) await validatedComponentSet(join(output, "bundles/sets", entry.publication_id), entry);
  assert.deepEqual(await readFile(join(previous!, "bundles/catalog.json")), before);
  await validatedComponentSet(join(previous!, "bundles/sets", nightlies[0].publication_id), nightlies[0]);
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
  const nightly = publicationFixture(3);
  const nightly_input = await componentSetFixture(directory, nightly);
  await writeFile(join(previous, "manual.html"), "preserve me");
  await assert.rejects(stageBundlePages(nightly, nightly_input, previous, output), /non-bundle Pages content/);
  await rm(join(previous, "manual.html"));
  await writeFile(join(previous, "bundles/channels/stable.json"), JSON.stringify({ schema_version: 1, channel: "stable", bundle_set_url: "https://example.com/wrong" }));
  await assert.rejects(stageBundlePages(nightly, nightly_input, previous, output), /disagree/);
});

test("release ordering follows semver rather than lexical or workflow run ordering", () => {
  for (const [low, high] of [["0.9.0", "0.10.0"], ["1.0.0-rc.2", "1.0.0-rc.10"], ["1.0.0-rc.10", "1.0.0"], ["1.0.0-1", "1.0.0-alpha"]]) assert.equal(compareVersions(low, high), -1);
  assert.equal(compareVersions("1.0.0+first", "1.0.0+second"), 0);
  assert.throws(() => compareVersions("1.0.0-01", "1.0.0"), /Invalid release version/);
});
