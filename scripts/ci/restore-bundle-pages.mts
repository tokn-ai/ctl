import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { appendFile, cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual, promisify } from "node:util";
import { bundleRoot, componentIdentity, repositoryName, validatedComponentSet } from "./bundle-publication.mts";
import { parseAgentBundleSet } from "../shared/agent-bundle.mts";
import { parseBundleCatalog, siteMarker, type BundleCatalog } from "./stage-bundle-pages.mts";
import { pages, type GhRunner } from "./update-draft-release.mts";

export const pagesJobName = "Publish bundle channels";
export const pagesDeployStep = "Deploy bundle Pages";
export const snapshotPrefix = "bundle-pages-state-";
type Fetch = (input: string | URL, init?: RequestInit) => Promise<Response>;

interface Artifact { id: number; name: string; expired: boolean; created_at: string; workflow_run: { id: number } }

/** Select by successful deployment step, not overall workflow/desktop outcome. */
export async function successfulSnapshot(repository: string, gh: GhRunner): Promise<Artifact | null> {
  const result: { artifacts: Artifact[] }[] = JSON.parse(await gh(["api", `repos/${repository}/actions/artifacts?per_page=100`, "--paginate", "--slurp"]));
  if (!Array.isArray(result) || result.some((page) => !Array.isArray(page.artifacts))) throw new Error("Invalid Actions artifact listing");
  const candidates = result.flatMap((page) => page.artifacts).filter((artifact) => !artifact.expired && /^bundle-pages-state-[1-9]\d*-[1-9]\d*$/.test(artifact.name))
    .sort((a, b) => b.created_at.localeCompare(a.created_at) || b.id - a.id);
  for (const artifact of candidates) {
    const match = /^bundle-pages-state-(\d+)-(\d+)$/.exec(artifact.name)!;
    if (String(artifact.workflow_run.id) !== match[1]) throw new Error("Pages snapshot provenance does not match its run");
    const run = JSON.parse(await gh(["api", `repos/${repository}/actions/runs/${match[1]}`]));
    if (!["push", "workflow_dispatch"].includes(run.event) || run.path !== ".github/workflows/bundles.yml" || run.head_repository?.full_name !== repository) continue;
    const jobs: { jobs: { name: string; steps: { name: string; conclusion: string | null }[] }[] }[] = JSON.parse(await gh([
      "api", `repos/${repository}/actions/runs/${match[1]}/attempts/${match[2]}/jobs?per_page=100`, "--paginate", "--slurp",
    ]));
    if (!Array.isArray(jobs) || jobs.some((page) => !Array.isArray(page.jobs))) throw new Error("Invalid Actions jobs listing");
    if (jobs.flatMap((page) => page.jobs).some((job) => job.name === pagesJobName && job.steps.some((step) => step.name === pagesDeployStep && step.conclusion === "success"))) return artifact;
  }
  return null;
}

async function download(url: URL, maximum: number, fetcher: Fetch, optional = false): Promise<Buffer | null> {
  url.searchParams.set("restore", randomUUID());
  const response = await fetcher(url, { redirect: "error", headers: { "Cache-Control": "no-cache" }, signal: AbortSignal.timeout(30_000) });
  if (optional && response.status === 404) return null;
  if (!response.ok || !response.body) throw new Error(`Could not restore published Pages file ${url.pathname}: HTTP ${response.status}`);
  const reader = response.body.getReader();
  const parts: Buffer[] = [];
  let size = 0;
  try {
    while (true) {
      const part = await reader.read();
      if (part.done) break;
      size += part.value.length;
      if (size > maximum) throw new Error(`Published Pages file exceeds its size limit: ${url.pathname}`);
      parts.push(Buffer.from(part.value));
    }
    return Buffer.concat(parts);
  } finally {
    await reader.cancel();
  }
}

async function restorePublished(catalog: BundleCatalog, directory: string, fetcher: Fetch): Promise<void> {
  const root = await download(new URL("../index.html", bundleRoot), 64 * 1024, fetcher);
  if (!root?.includes(`<!-- ${siteMarker} -->`)) throw new Error("Refusing to replace an existing non-managed Pages site");
  await mkdir(join(directory, "bundles/channels"), { recursive: true });
  await writeFile(join(directory, "index.html"), root);
  await writeFile(join(directory, ".nojekyll"), "");
  await writeFile(join(directory, "bundles/catalog.json"), JSON.stringify(catalog));
  for (const entry of catalog.publications) {
    const relative = `sets/${entry.publication_id}/`;
    const local = join(directory, "bundles", relative);
    await mkdir(local, { recursive: true });
    const manifest_bytes = await download(new URL(`${relative}bundle-set.json`, bundleRoot), 64 * 1024, fetcher);
    await writeFile(join(local, "bundle-set.json"), manifest_bytes!);
    // Validate names and identity before using archive filenames in URLs/paths.
    const parsed = parseAgentBundleSet(manifest_bytes!, componentIdentity(entry));
    if (parsed.schema_version !== 2) throw new Error("Published Pages set does not use schema 2");
    for (const target of Object.values(parsed.targets)) {
      for (const [name, maximum] of [[target.archive, 128 * 1024 * 1024], [`${target.archive}.sha256`, 256]] as const) {
        await writeFile(join(local, name), (await download(new URL(`${relative}${name}`, bundleRoot), maximum, fetcher))!);
      }
    }
    await validatedComponentSet(local, entry);
  }
  for (const channel of ["dev", "stable"] as const) {
    if (catalog.channels[channel]) await writeFile(join(directory, "bundles/channels", `${channel}.json`), (await download(new URL(`channels/${channel}.json`, bundleRoot), 4096, fetcher))!);
  }
}

async function previouslyPublished(repository: string, gh: GhRunner): Promise<boolean> {
  const deployments = await pages<{ id: number }>(gh, `repos/${repository}/deployments?environment=github-pages&per_page=100`);
  for (const deployment of deployments) {
    if (!Number.isSafeInteger(deployment.id) || deployment.id < 1) throw new Error("Invalid Pages deployment history");
    const statuses = await pages<{ state?: unknown } | null>(gh, `repos/${repository}/deployments/${deployment.id}/statuses?per_page=100`);
    // Actions records waiting before queued/in_progress for an environment job;
    // none of those states establishes that content was published.
    const known = new Set(["error", "failure", "inactive", "in_progress", "queued", "pending", "success", "waiting"]);
    for (const status of statuses) {
      if (typeof status?.state !== "string" || !known.has(status.state)) {
        throw new Error(`Invalid Pages deployment status for deployment ${deployment.id}: ${JSON.stringify(status?.state)}`);
      }
    }
    // Current environment jobs exist before they publish anything. An inactive
    // deployment may have lost its older success status to API retention.
    if (statuses.some((status) => status?.state === "success" || status?.state === "inactive")) return true;
  }
  return false;
}

/** Missing/transient/corrupt state never silently becomes an empty deployment. */
export async function restoreBundlePages(repository: string, output: string, gh: GhRunner, fetcher: Fetch = fetch): Promise<boolean> {
  repositoryName(repository);
  const site = JSON.parse(await gh(["api", `repos/${repository}/pages`]));
  const expected = new URL("../", bundleRoot);
  let actual: URL | null = null;
  try { if (typeof site.html_url === "string") actual = new URL(site.html_url); } catch { /* Reject malformed configuration below. */ }
  if (actual?.protocol === "http:") actual.protocol = "https:";
  if (site.build_type !== "workflow" || (site.cname && site.cname !== expected.hostname) || actual?.href !== expected.href) {
    throw new Error(`Pages must use GitHub Actions at ${expected.href}; refusing to replace another publishing source or custom site`);
  }
  const temp = await mkdtemp(join(tmpdir(), "ctl-bundle-pages-"));
  try {
    const artifact = await successfulSnapshot(repository, gh);
    let artifact_catalog: BundleCatalog | null = null;
    const snapshot = join(temp, "snapshot");
    if (artifact) {
      await gh(["run", "download", String(artifact.workflow_run.id), "--repo", repository, "--name", artifact.name, "--dir", snapshot]);
      artifact_catalog = parseBundleCatalog(await readFile(join(snapshot, "bundles/catalog.json")));
      const match = /^bundle-pages-state-(\d+)-(\d+)$/.exec(artifact.name)!;
      if (!artifact_catalog.publications.some((entry) => entry.run_id === match[1] && entry.run_attempt === Number(match[2]))) throw new Error("Downloaded Pages snapshot does not belong to its deployment attempt");
    }
    const published_bytes = await download(new URL("catalog.json", bundleRoot), 64 * 1024, fetcher, true);
    const published = published_bytes ? parseBundleCatalog(published_bytes) : null;
    if (artifact_catalog && published && artifact_catalog.generation === published.generation && !isDeepStrictEqual(artifact_catalog, published)) {
      throw new Error("Pages snapshot and published catalog have conflicting content at the same generation");
    }
    if (artifact_catalog && (!published || artifact_catalog.generation >= published.generation)) {
      await cp(snapshot, output, { recursive: true, force: false, errorOnExist: true });
      return true;
    }
    if (published) {
      const restored = join(temp, "published");
      await restorePublished(published, restored, fetcher);
      await cp(restored, output, { recursive: true, force: false, errorOnExist: true });
      return true;
    }
    const deployed = await previouslyPublished(repository, gh);
    const root = await download(new URL("../index.html", bundleRoot), 64 * 1024, fetcher, true);
    if (deployed || root !== null) throw new Error("Existing Pages deployment has no managed catalog; refusing an empty-site replacement");
    return false;
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const [output, ...extra] = process.argv.slice(2);
  if (!output || extra.length) throw new Error("usage: restore-bundle-pages.mts OUTPUT");
  const run = promisify(execFile);
  const restored = await restoreBundlePages(process.env.GH_REPO ?? process.env.GITHUB_REPOSITORY ?? "", resolve(output),
    async (args) => (await run("gh", args, { maxBuffer: 16 * 1024 * 1024 })).stdout);
  if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, `previous=${restored ? resolve(output) : "-"}\n`);
  console.log(restored ? "Restored the last complete bundle Pages site" : "No previous Pages deployment exists; starting the managed bundle site");
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
