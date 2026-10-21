import { appendFile, copyFile, cp, lstat, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { bundleRoot, validatedComponentSet, type BundleChannel } from "./bundle-publication.mts";
import { validateAgentIdentity, type AgentBundleIdentity } from "../shared/agent-bundle.mts";

export const siteMarker = "ctl-bundle-pages";
export const devRetention = 5;
const stableRetention = 2;
const maximumSiteBytes = 900 * 1024 * 1024;

export interface Publication extends AgentBundleIdentity {
  publication_id: string;
  channel: BundleChannel;
  run_id: string;
  run_number: number;
  run_attempt: number;
}
export interface BundleCatalog {
  schema_version: 1;
  site: typeof siteMarker;
  generation: number;
  publications: Publication[];
  channels: Partial<Record<BundleChannel, string>>;
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validatePublication(value: unknown): asserts value is Publication {
  if (!object(value)) throw new Error("Invalid bundle publication");
  if (["app_version", "bundle_id", "git_revision"].some((field) => typeof value[field] !== "string")) throw new Error("Invalid bundle publication identity");
  validateAgentIdentity(value as unknown as AgentBundleIdentity);
  if ((value.channel !== "dev" && value.channel !== "stable") || typeof value.run_id !== "string" || !/^[1-9]\d*$/.test(value.run_id) ||
    !Number.isSafeInteger(value.run_number) || (value.run_number as number) <= 0 || !Number.isSafeInteger(value.run_attempt) || (value.run_attempt as number) <= 0 ||
    value.publication_id !== `${value.bundle_id}-${value.run_id}-${value.run_attempt}` ||
    (value.channel === "stable") !== (value.app_version === value.bundle_id)) throw new Error("Invalid bundle publication identity or channel");
}

export function parseBundleCatalog(bytes: Buffer): BundleCatalog {
  if (bytes.length > 64 * 1024) throw new Error("Pages catalog exceeds its size limit");
  const value: unknown = JSON.parse(bytes.toString("utf8"));
  if (!object(value) || value.schema_version !== 1 || value.site !== siteMarker || !Number.isSafeInteger(value.generation) || (value.generation as number) < 1 ||
    !Array.isArray(value.publications) || value.publications.length > devRetention + stableRetention || !object(value.channels) ||
    Object.keys(value.channels).some((key) => key !== "dev" && key !== "stable")) throw new Error("Existing Pages site is not a supported managed bundle site");
  const seen = new Set<string>();
  for (const publication of value.publications) {
    validatePublication(publication);
    if (seen.has(publication.publication_id)) throw new Error("Duplicate Pages publication");
    seen.add(publication.publication_id);
  }
  for (const [channel, id] of Object.entries(value.channels)) {
    if (!value.publications.some((entry) => entry.publication_id === id && entry.channel === channel && (channel !== "stable" || !semver(entry.app_version).prerelease.length))) throw new Error("Pages channel points outside its retained publication set or to a prerelease");
  }
  return value as unknown as BundleCatalog;
}

function semver(value: string): { core: bigint[]; prerelease: string[] } {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/.exec(value);
  if (!match) throw new Error(`Invalid release version: ${value}`);
  const prerelease = match[4]?.split(".") ?? [];
  if (prerelease.some((part) => /^0\d+$/.test(part))) throw new Error(`Invalid release version: ${value}`);
  return { core: match.slice(1, 4).map(BigInt), prerelease };
}

export function compareVersions(left: string, right: string): number {
  const a = semver(left), b = semver(right);
  for (let index = 0; index < 3; index += 1) if (a.core[index] !== b.core[index]) return a.core[index] > b.core[index] ? 1 : -1;
  if (!a.prerelease.length || !b.prerelease.length) return Number(!a.prerelease.length) - Number(!b.prerelease.length);
  for (let index = 0; index < Math.max(a.prerelease.length, b.prerelease.length); index += 1) {
    const x = a.prerelease[index], y = b.prerelease[index];
    if (x === undefined || y === undefined) return Number(x !== undefined) - Number(y !== undefined);
    if (x === y) continue;
    const xn = /^\d+$/.test(x), yn = /^\d+$/.test(y);
    if (xn && yn) return BigInt(x) > BigInt(y) ? 1 : -1;
    if (xn !== yn) return xn ? -1 : 1;
    return x > y ? 1 : -1;
  }
  return 0;
}

function newer(left: Publication, right: Publication): number {
  return (left.channel === "stable" ? compareVersions(left.app_version, right.app_version) : 0) || left.run_number - right.run_number || left.run_attempt - right.run_attempt;
}

async function regularTree(path: string): Promise<number> {
  const info = await lstat(path);
  if (info.isFile()) return info.size;
  if (!info.isDirectory()) throw new Error(`Pages contains a symlink or non-regular entry: ${path}`);
  let bytes = 0;
  for (const name of await readdir(path)) bytes += await regularTree(join(path, name));
  return bytes;
}

/** Fail closed before touching output; each pointer is built with all its archives. */
export async function stageBundlePages(publication: Publication, incoming: string, previous: string | null, output: string, maximum_bytes = maximumSiteBytes): Promise<boolean> {
  validatePublication(publication);
  await validatedComponentSet(incoming, publication);
  let catalog: BundleCatalog = { schema_version: 1, site: siteMarker, generation: 0, publications: [], channels: {} };
  if (previous) {
    catalog = parseBundleCatalog(await readFile(join(previous, "bundles/catalog.json")));
    const unknown = (await readdir(previous)).filter((name) => !["bundles", "index.html", ".nojekyll"].includes(name));
    if (unknown.length) throw new Error(`Refusing to replace non-bundle Pages content: ${unknown.join(", ")}`);
    if (!(await readFile(join(previous, "index.html"), "utf8")).includes(`<!-- ${siteMarker} -->`)) throw new Error("Refusing to replace a non-managed Pages index");
    if ((await readdir(join(previous, "bundles"))).some((name) => !["catalog.json", "channels", "sets"].includes(name)) ||
      (await readdir(join(previous, "bundles/sets"))).some((name) => !catalog.publications.some((entry) => entry.publication_id === name)) ||
      (await readdir(join(previous, "bundles/channels"))).some((name) => !Object.keys(catalog.channels).some((channel) => name === `${channel}.json`))) throw new Error("Refusing to replace unrecognized content in the bundle Pages site");
    await regularTree(previous);
    for (const entry of catalog.publications) {
      const directory = join(previous, "bundles/sets", entry.publication_id);
      const { asset_names } = await validatedComponentSet(directory, entry);
      if ((await readdir(directory)).some((name) => !asset_names.includes(name))) throw new Error("Unrecognized content in an immutable bundle set");
    }
    for (const channel of ["dev", "stable"] as const) {
      if (!catalog.channels[channel]) continue;
      const pointer = JSON.parse(await readFile(join(previous, "bundles/channels", `${channel}.json`), "utf8"));
      if (pointer.schema_version !== 1 || pointer.channel !== channel || pointer.bundle_set_url !== `${bundleRoot}sets/${catalog.channels[channel]}/bundle-set.json`) throw new Error("Pages channel and catalog disagree");
    }
  }
  const current = catalog.publications.find((entry) => entry.publication_id === catalog.channels[publication.channel]);
  if (publication.channel === "dev" && current && newer(publication, current) <= 0) return false;
  if (current?.channel === "stable" && current.app_version === publication.app_version && current.git_revision !== publication.git_revision) throw new Error("A release version cannot change source revision");
  if (catalog.publications.some((entry) => entry.publication_id === publication.publication_id)) return false;
  const promote = publication.channel === "dev" || (!semver(publication.app_version).prerelease.length && (!current || newer(publication, current) > 0));
  const retained = [...catalog.publications, publication];
  const channels = promote ? { ...catalog.channels, [publication.channel]: publication.publication_id } : catalog.channels;
  const pinned_stable = retained.find((entry) => entry.publication_id === channels.stable);
  const stable = retained.filter((entry) => entry.channel === "stable" && entry !== pinned_stable)
    .sort((a, b) => b.run_number - a.run_number || b.run_attempt - a.run_attempt).slice(0, stableRetention - Number(!!pinned_stable));
  if (pinned_stable) stable.unshift(pinned_stable);
  catalog = { ...catalog, generation: catalog.generation + 1, channels,
    publications: [
      ...retained.filter((entry) => entry.channel === "dev").sort((a, b) => newer(b, a)).slice(0, devRetention),
      ...stable,
    ] };
  if (!catalog.publications.includes(publication)) return false;
  // Stage into a fresh sibling directory. Never mutate the restored snapshot.
  if (await lstat(output).then(() => true, (error: NodeJS.ErrnoException) => { if (error.code === "ENOENT") return false; throw error; })) throw new Error("Pages output directory already exists");
  try {
    if (previous) await cp(previous, output, { recursive: true, errorOnExist: true, force: false });
    else await mkdir(output, { recursive: true });
    const sets = join(output, "bundles/sets");
    await mkdir(sets, { recursive: true });
    const keep = new Set(catalog.publications.map((entry) => entry.publication_id));
    for (const name of await readdir(sets)) if (!keep.has(name)) await rm(join(sets, name), { recursive: true, force: true });
    const next = join(sets, publication.publication_id);
    await mkdir(next);
    const { asset_names } = await validatedComponentSet(incoming, publication);
    for (const name of asset_names) await copyFile(join(incoming, name), join(next, name));
    await mkdir(join(output, "bundles/channels"), { recursive: true });
    for (const channel of ["dev", "stable"] as const) {
      const id = catalog.channels[channel];
      if (id) await writeFile(join(output, "bundles/channels", `${channel}.json`), `${JSON.stringify({ schema_version: 1, channel, bundle_set_url: `${bundleRoot}sets/${id}/bundle-set.json` }, null, 2)}\n`);
    }
    await writeFile(join(output, "bundles/catalog.json"), `${JSON.stringify(catalog, null, 2)}\n`);
    await writeFile(join(output, ".nojekyll"), "");
    await writeFile(join(output, "index.html"), `<!-- ${siteMarker} --><!doctype html><title>ctl component bundles</title><h1>ctl component bundles</h1><p><a href="bundles/channels/dev.json">Dev channel</a> · <a href="bundles/channels/stable.json">Stable channel</a></p><p>Channel documents reference verified bundle sets and downloadable archives.</p>\n`);
    if (await regularTree(output) > maximum_bytes) throw new Error("Retained bundle site exceeds the Pages safety limit (900 MiB in CI)");
    return true;
  } catch (error) {
    await rm(output, { recursive: true, force: true });
    throw error;
  }
}

async function main(): Promise<void> {
  const [channel, app_version, bundle_id, git_revision, incoming, previous, output, ...extra] = process.argv.slice(2);
  if ((channel !== "dev" && channel !== "stable") || !output || extra.length) throw new Error("usage: stage-bundle-pages.mts dev|stable APP_VERSION BUNDLE_ID GIT_REVISION INPUT PREVIOUS|- OUTPUT");
  const run_id = process.env.GITHUB_RUN_ID ?? "";
  const run_attempt = Number(process.env.GITHUB_RUN_ATTEMPT);
  const publication: Publication = { channel, app_version, bundle_id, git_revision, run_id, run_attempt,
    run_number: Number(process.env.GITHUB_RUN_NUMBER), publication_id: `${bundle_id}-${run_id}-${run_attempt}` };
  const changed = await stageBundlePages(publication, resolve(incoming), previous === "-" ? null : resolve(previous), resolve(output));
  console.log(changed ? `Staged ${publication.publication_id}` : "A newer channel publication already exists; skipping deployment");
  if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, `publish=${changed}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
