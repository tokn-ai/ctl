import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { validatedComponentSet } from "./bundle-publication.mts";
import { componentSetFixture, publicationFixture } from "./bundle-publication.fixtures.mts";
import { regularTree, stageBundlePages } from "./stage-bundle-pages.mts";
import { stagePagesLanding } from "./stage-pages-landing.mts";

async function fixture(t: TestContext) {
  const directory = await mkdtemp(join(tmpdir(), "ctl-stage-pages-landing-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const site = join(directory, "site");
  const landing = join(directory, "landing");
  const stable = publicationFixture(1, "stable");
  await stageBundlePages(stable, await componentSetFixture(directory, stable), null, site);
  await mkdir(join(landing, "assets"), { recursive: true });
  const index = "<!doctype html><title>Welcome to ctl</title>";
  await writeFile(join(landing, "index.html"), index);
  await writeFile(join(landing, "assets", "site.css"), "body { color: black; }");
  return { directory, site, landing, stable, index };
}

test("a Pages artifact keeps the installed bundle paths alongside the landing page", async (t) => {
  const { site, landing, stable, index } = await fixture(t);
  const pointer = join(site, "bundles/channels/stable.json");
  const expected_pointer = await readFile(pointer);

  await stagePagesLanding(site, landing);

  assert.equal(await readFile(join(site, "index.html"), "utf8"), `<!-- ctl-bundle-pages -->\n${index}`);
  assert.equal(await readFile(join(site, "assets", "site.css"), "utf8"), "body { color: black; }");
  assert.deepEqual(await readFile(pointer), expected_pointer);
  assert.equal(await readFile(join(site, ".nojekyll"), "utf8"), "");
  assert.equal(await readFile(join(landing, "index.html"), "utf8"), index);
  await validatedComponentSet(join(site, "bundles/sets", stable.publication_id), stable);
});

test("rejects reserved paths and linked landing assets before overlaying the site", async (t) => {
  const { directory, site, landing } = await fixture(t);
  const bundle_index = await readFile(join(site, "index.html"));
  await mkdir(join(landing, "bundles"));
  await assert.rejects(stagePagesLanding(site, landing), /conflicts with the bundle site: bundles/);
  await rm(join(landing, "bundles"), { recursive: true });
  await writeFile(join(landing, ".nojekyll"), "replacement");
  await assert.rejects(stagePagesLanding(site, landing), /conflicts with the bundle site: \.nojekyll/);
  await rm(join(landing, ".nojekyll"));
  const outside = join(directory, "unrelated");
  await mkdir(outside);
  await symlink(outside, join(landing, "assets", "linked"), process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(stagePagesLanding(site, landing), /symlink or non-regular entry/);

  assert.deepEqual(await readFile(join(site, "index.html")), bundle_index);
  assert.equal(await readFile(join(site, ".nojekyll"), "utf8"), "");
  await assert.rejects(readFile(join(site, "assets", "site.css")), { code: "ENOENT" });
});

test("counts landing assets toward the deployment size limit before writing them", async (t) => {
  const { site, landing } = await fixture(t);
  const bundle_index = await readFile(join(site, "index.html"));
  const bundle_bytes = await regularTree(site);
  await writeFile(join(landing, "assets", "large.png"), Buffer.alloc(1024));

  await assert.rejects(stagePagesLanding(site, landing, bundle_bytes), /exceed the Pages safety limit/);

  assert.deepEqual(await readFile(join(site, "index.html")), bundle_index);
  assert.equal(await regularTree(site), bundle_bytes);
});
