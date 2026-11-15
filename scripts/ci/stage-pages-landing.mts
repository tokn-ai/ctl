import { randomUUID } from "node:crypto";
import { cp, lstat, readFile, readdir, rename, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { maximumSiteBytes, regularTree, siteMarker } from "./stage-bundle-pages.mts";

const marker = `<!-- ${siteMarker} -->`;
const maximum_index_bytes = 64 * 1024;
const reserved = new Set(["bundles", ".nojekyll", "cname", "index.html"]);

/** Add the landing page to an already staged, verified bundle site. */
export async function stagePagesLanding(bundle_site: string, landing_dist: string, maximum_bytes = maximumSiteBytes): Promise<void> {
  if (!(await lstat(bundle_site)).isDirectory() || !(await lstat(landing_dist)).isDirectory()) {
    throw new Error("The bundle site and landing build must be regular directories");
  }
  const bundle_index = join(bundle_site, "index.html");
  const bundle_index_info = await lstat(bundle_index);
  if (!(await lstat(join(bundle_site, "bundles"))).isDirectory() ||
      !(await lstat(join(bundle_site, ".nojekyll"))).isFile() ||
      !bundle_index_info.isFile() ||
      !(await readFile(bundle_index, "utf8")).includes(marker)) {
    throw new Error("The destination must contain a managed bundle site");
  }
  const landing_index = join(landing_dist, "index.html");
  const index_info = await lstat(landing_index);
  if (!index_info.isFile() || index_info.size === 0 || index_info.size > maximum_index_bytes) {
    throw new Error("The landing build must contain a regular index.html within the Pages index size limit");
  }
  const index = await readFile(landing_index, "utf8");
  // The public fallback in restore-bundle-pages recognizes the managed site by
  // this marker, even when the root page is the landing page.
  const managed_index = index.includes(marker) ? index : `${marker}\n${index}`;
  const managed_index_bytes = Buffer.byteLength(managed_index);
  if (managed_index_bytes > maximum_index_bytes) throw new Error("The combined Pages index exceeds its size limit");

  const assets = (await readdir(landing_dist)).filter((name) => name !== "index.html");
  const existing = new Set((await readdir(bundle_site)).map((name) => name.toLowerCase()));
  let site_bytes = await regularTree(bundle_site) - bundle_index_info.size + managed_index_bytes;
  for (const name of assets) {
    const key = name.toLowerCase();
    if (reserved.has(key) || existing.has(key)) throw new Error(`Landing asset conflicts with the bundle site: ${name}`);
    existing.add(key);
    site_bytes += await regularTree(join(landing_dist, name));
  }
  if (site_bytes > maximum_bytes) throw new Error("Landing page and bundles exceed the Pages safety limit (900 MiB in CI)");
  for (const name of assets) {
    await cp(join(landing_dist, name), join(bundle_site, name), { recursive: true, force: false, errorOnExist: true });
  }
  const temporary_index = join(bundle_site, `.landing-index-${randomUUID()}`);
  try {
    await writeFile(temporary_index, managed_index, { flag: "wx" });
    await rename(temporary_index, bundle_index);
  } finally {
    await rm(temporary_index, { force: true });
  }
}

async function main(): Promise<void> {
  const [bundle_site, landing_dist, ...extra] = process.argv.slice(2);
  if (!bundle_site || !landing_dist || extra.length) throw new Error("usage: stage-pages-landing.mts BUNDLE_SITE LANDING_DIST");
  await stagePagesLanding(resolve(bundle_site), resolve(landing_dist));
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
