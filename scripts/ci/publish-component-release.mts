import { execFile } from "node:child_process";
import { copyFile, lstat, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { componentIdentity, fileDigest, repositoryName, validatedComponentSet } from "./bundle-publication.mts";
import { parseAgentBundleSet } from "../shared/agent-bundle.mts";
import { findRelease, managedLabelPrefix, mergeReleaseNotes, pages, releaseNotes, validateReleaseBundle, verifyReleaseTag,
  type BundleIdentity, type GhRunner } from "./update-draft-release.mts";

interface Asset { id: number; name: string; label: string | null; digest?: string | null; size: number }
interface Release { id: number; draft: boolean; tag_name: string; body: string | null; html_url: string; immutable?: boolean }

/** Components publish independently; later desktop builds only append verified assets. */
export async function publishComponentRelease(repository: string, build: BundleIdentity, directory: string, mode: "components" | "desktop", gh: GhRunner): Promise<string> {
  const identity = componentIdentity(build);
  repositoryName(repository);
  if (identity.bundle_id !== identity.app_version) throw new Error("Only version-tag bundles may publish a release");
  const components = await validatedComponentSet(directory, identity);
  let desktop = mode === "desktop" ? await validateReleaseBundle(identity, directory) : null;
  const names = desktop?.asset_names ?? components.asset_names;
  const tag = `v${identity.app_version}`;
  await verifyReleaseTag(gh, repository, tag, identity.git_revision, true);
  let release: Release | undefined = await findRelease(gh, repository, tag);
  if (mode === "desktop" && (!release || release.draft)) throw new Error("Component release must be published before desktop assets are appended");
  const temp = await mkdtemp(join(tmpdir(), "ctl-component-release-"));
  const payload = join(temp, "release.json");
  const component_notes = ["<!-- ctmux-ci:start -->", `Built from commit \`${identity.git_revision}\`.`, `Bundle ID: \`${identity.bundle_id}\`.`, "",
    "Contains verified Linux and macOS component bundles for Intel/AMD and ARM64.",
    "Desktop packages and signed macOS helpers are attached separately when those builds succeed.", "<!-- ctmux-ci:end -->"].join("\n");
  const notes = (body: string | null): string => desktop ? releaseNotes(desktop, body) : mergeReleaseNotes(component_notes, body);
  try {
    // Reject malformed managed notes before creating or uploading anything.
    notes(release?.body ?? null);
    if (!release) {
      await writeFile(payload, JSON.stringify({ tag_name: tag, target_commitish: identity.git_revision, name: `ctmux ${tag}`, draft: true, body: notes(null) }));
      release = JSON.parse(await gh(["api", `repos/${repository}/releases`, "--method", "POST", "--input", payload])) as Release;
      if (!Number.isSafeInteger(release.id) || !release.draft || release.tag_name !== tag || typeof release.html_url !== "string") throw new Error("GitHub did not return the expected component draft");
    }
    const endpoint = `repos/${repository}/releases/${release.id}`;
    const assets = await pages<Asset>(gh, `${endpoint}/assets?per_page=100`);
    if (!release.draft && (mode === "components" || names.every((name) => assets.some((asset) => asset.name === name)))) {
      // Tar metadata and signing can change on a fresh build. The first
      // published set is canonical for this tag, so reuse its verified bytes
      // instead of replacing them or making a harmless retry fail.
      const canonical = join(temp, "canonical");
      await mkdir(canonical);
      await gh(["release", "download", tag, "--repo", repository, "--dir", canonical, "--pattern", "bundle-set.json"]);
      const manifest = parseAgentBundleSet(await readFile(join(canonical, "bundle-set.json")), identity);
      const download_names = mode === "desktop" ? names.filter((name) => name !== "bundle-set.json") : Object.values(manifest.targets).flatMap((entry) => [entry.archive, `${entry.archive}.sha256`]);
      await gh(["release", "download", tag, "--repo", repository, "--dir", canonical, ...download_names.flatMap((name) => ["--pattern", name])]);
      const verified = mode === "desktop" ? await validateReleaseBundle(identity, canonical) : await validatedComponentSet(canonical, identity);
      for (const name of verified.asset_names) {
        const asset = assets.find((entry) => entry.name === name);
        if (!asset?.label?.startsWith(managedLabelPrefix) || asset.digest !== await fileDigest(join(canonical, name)) || asset.size !== (await lstat(join(canonical, name))).size) {
          throw new Error(`Published canonical release asset does not match its immutable digest: ${name}`);
        }
      }
      for (const name of verified.asset_names) await copyFile(join(canonical, name), join(directory, name));
      if (mode === "desktop") desktop = await validateReleaseBundle(identity, directory);
    }
    const matches = new Set<string>();
    const local = new Map<string, { digest: string; size: number }>();
    for (const name of names) local.set(name, { digest: await fileDigest(join(directory, name)), size: (await lstat(join(directory, name))).size });
    if (!release.draft && !assets.some((asset) => asset.name === "bundle-set.json" && asset.label?.startsWith(managedLabelPrefix) &&
      asset.digest === local.get("bundle-set.json")!.digest && asset.size === local.get("bundle-set.json")!.size)) {
      throw new Error("Published release has no matching immutable bundle-set.json; refusing assets from another build");
    }
    // Preflight every name before the first mutation; published bytes are immutable.
    for (const name of names) {
      const existing = assets.find((asset) => asset.name === name);
      if (!existing) continue;
      if (!existing.label?.startsWith(managedLabelPrefix)) throw new Error(`Refusing to overwrite manually managed release asset: ${name}`);
      const identical = existing.digest === local.get(name)!.digest && existing.size === local.get(name)!.size;
      if (identical) matches.add(name);
      else if (!release.draft) throw new Error(`Published asset differs from this build and cannot be replaced: ${name}`);
    }
    if (release.immutable && names.some((name) => !matches.has(name))) throw new Error("GitHub release immutability prevents appending desktop assets after component publication; no release policy was changed");
    for (const name of names) {
      if (matches.has(name)) continue;
      const existing = assets.find((asset) => asset.name === name);
      if (existing) {
        const current = JSON.parse(await gh(["api", endpoint])) as Release;
        if (!current.draft) throw new Error("Release was published during asset replacement; refusing to modify it");
        await gh(["api", `repos/${repository}/releases/assets/${existing.id}`, "--method", "DELETE"]);
      }
      const query = `name=${encodeURIComponent(name)}&label=${encodeURIComponent(`${managedLabelPrefix}${name}`)}`;
      const uploaded = JSON.parse(await gh(["api", `https://uploads.github.com/repos/${repository}/releases/${release.id}/assets?${query}`,
        "--method", "POST", "--input", join(directory, name), "--header", "Content-Type: application/octet-stream"]));
      if (uploaded.name !== name || uploaded.state !== "uploaded" || uploaded.digest !== local.get(name)!.digest || uploaded.size !== local.get(name)!.size) {
        throw new Error(`GitHub did not confirm a complete matching release asset: ${name}`);
      }
    }
    const latest = JSON.parse(await gh(["api", endpoint])) as Release;
    if (release.draft) {
      if (!latest.draft) throw new Error("Release was published before component assembly completed; inspect its assets before retrying");
      // A main draft may contain obsolete development desktops. Do not expose
      // them under the stable version; successful tag builds append later.
      for (const asset of assets.filter((asset) => asset.label?.startsWith(managedLabelPrefix) && !names.includes(asset.name))) {
        await gh(["api", `repos/${repository}/releases/assets/${asset.id}`, "--method", "DELETE"]);
      }
    }
    if (release.draft || (desktop && notes(latest.body) !== latest.body)) {
      const prerelease = /^\d+\.\d+\.\d+-/.test(identity.app_version);
      await writeFile(payload, JSON.stringify({ tag_name: tag, body: notes(latest.body),
        ...(release.draft ? { draft: false, prerelease, make_latest: prerelease ? "false" : "legacy" } : {}) }));
      await gh(["api", endpoint, "--method", "PATCH", "--input", payload]);
    }
    return release.html_url;
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const [mode, app_version, bundle_id, git_revision, directory, ...extra] = process.argv.slice(2);
  if ((mode !== "components" && mode !== "desktop") || !directory || extra.length) throw new Error("usage: publish-component-release.mts components|desktop APP_VERSION BUNDLE_ID GIT_REVISION ASSET_DIRECTORY");
  const run = promisify(execFile);
  const url = await publishComponentRelease(process.env.GH_REPO ?? process.env.GITHUB_REPOSITORY ?? "", { app_version, bundle_id, git_revision }, resolve(directory), mode,
    async (args) => (await run("gh", args, { maxBuffer: 16 * 1024 * 1024 })).stdout);
  console.log(`Published component release: ${url}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
