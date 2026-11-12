import { execFile } from "node:child_process";
import { copyFile, lstat, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { componentIdentity, fileDigest, repositoryName, validatedComponentSet } from "./bundle-publication.mts";
import { archiveName, parseAgentBundleSet, windowsAgentManifestName, windowsAgentTarget } from "../shared/agent-bundle.mts";
import { updaterBinaryName, updaterManifestName } from "../shared/updater-bundle.mts";
import { findRelease, managedLabelPrefix, mergeReleaseNotes, pages, releaseNotes, validateReleaseBundle, verifyReleaseTag,
  type BundleIdentity, type GhRunner } from "./update-draft-release.mts";

interface Asset { id: number; name: string; label: string | null; digest?: string | null; size: number }
interface Release { id: number; draft: boolean; tag_name: string; body: string | null; html_url: string; immutable?: boolean }

async function verifyPublishedAsset(asset: Asset, directory: string): Promise<void> {
  if (!asset.label?.startsWith(managedLabelPrefix) || asset.digest !== await fileDigest(join(directory, asset.name)) ||
    asset.size !== (await lstat(join(directory, asset.name))).size) {
    throw new Error(`Published canonical release asset does not match its immutable digest: ${asset.name}`);
  }
}

function additionalManifestName(name: string, bundle_id: string): string | undefined {
  if (/^(desktop-(?:x86_64|aarch64)-(?:unknown-linux-gnu|apple-darwin)|(?:ctld|ctl-cli)-(?:x86_64|aarch64)-apple-darwin)\.json$/.test(name)) return name;
  const packages: [string, RegExp, string][] = [
    [`ctmux-${bundle_id}-`, /^((?:x86_64|aarch64)-unknown-linux-gnu)\.(?:deb|rpm|AppImage)(?:\.sha256)?$/, "desktop"],
    [`ctmux-${bundle_id}-`, /^((?:x86_64|aarch64)-apple-darwin)\.(?:dmg|app\.tar\.gz)(?:\.sha256)?$/, "desktop"],
    [`ctld-${bundle_id}-`, /^((?:x86_64|aarch64)-apple-darwin)\.app\.tar\.gz(?:\.sha256)?$/, "ctld"],
    [`ctl-${bundle_id}-`, /^((?:x86_64|aarch64)-apple-darwin)\.tar\.gz(?:\.sha256)?$/, "ctl-cli"],
  ];
  for (const [prefix, suffix, component] of packages) {
    if (!name.startsWith(prefix)) continue;
    const match = suffix.exec(name.slice(prefix.length));
    if (match) return `${component}-${match[1]}.json`;
  }
  return undefined;
}

/** Restore published bytes per target, including targets absent from this run. */
async function restoreCanonicalAssets(repository: string, identity: BundleIdentity, directory: string, mode: "components" | "desktop",
  assets: Asset[], canonical: string, gh: GhRunner): Promise<void> {
  const incoming = mode === "desktop"
    ? await validateReleaseBundle(identity, directory, { allow_partial: true })
    : await validatedComponentSet(directory, identity);
  await mkdir(canonical);
  for (const name of incoming.asset_names) await copyFile(join(directory, name), join(canonical, name));
  const download = async (names: string[]): Promise<void> => {
    if (names.length) await gh(["release", "download", `v${identity.app_version}`, "--repo", repository, "--dir", canonical,
      "--clobber", ...names.flatMap((name) => ["--pattern", name])]);
  };
  await download(["bundle-set.json"]);
  await verifyPublishedAsset(assets.find((asset) => asset.name === "bundle-set.json")!, canonical);
  const manifest = parseAgentBundleSet(await readFile(join(canonical, "bundle-set.json")), identity);
  const remote_names = Object.values(manifest.targets).flatMap((entry) => [entry.archive, `${entry.archive}.sha256`]);
  for (const target of Object.keys(manifest.targets)) {
    const binary = updaterBinaryName(identity, target);
    remote_names.push(binary, `${binary}.sha256`, updaterManifestName(target));
  }
  const windows_archive = archiveName(identity, windowsAgentTarget);
  const windows_updater = updaterBinaryName(identity, windowsAgentTarget);
  remote_names.push(windowsAgentManifestName, windows_archive, `${windows_archive}.sha256`,
    windows_updater, `${windows_updater}.sha256`, updaterManifestName(windowsAgentTarget));
  const target_manifests = new Set([...incoming.asset_names, ...assets.map((asset) => asset.name)]
    .filter((name) => additionalManifestName(name, identity.bundle_id) === name));
  // A failed upload may have left a subset. Local files can fill missing names
  // only when they validate against the already-published canonical manifest.
  await download(assets.filter((asset) => remote_names.includes(asset.name) ||
    (mode === "desktop" && target_manifests.has(additionalManifestName(asset.name, identity.bundle_id) ?? ""))).map((asset) => asset.name));
  const verified = mode === "desktop"
    ? await validateReleaseBundle(identity, canonical, { allow_partial: true })
    : await validatedComponentSet(canonical, identity);
  for (const asset of assets.filter((asset) => verified.asset_names.includes(asset.name))) await verifyPublishedAsset(asset, canonical);
  for (const name of verified.asset_names) await copyFile(join(canonical, name), join(directory, name));
}

/** Components publish independently; later desktop builds only append verified assets. */
export async function publishComponentRelease(repository: string, build: BundleIdentity, directory: string, mode: "components" | "desktop", gh: GhRunner): Promise<string> {
  const identity = componentIdentity(build);
  repositoryName(repository);
  if (identity.bundle_id !== identity.app_version) throw new Error("Only version-tag bundles may publish a release");
  const components = await validatedComponentSet(directory, identity);
  let desktop = mode === "desktop" ? await validateReleaseBundle(identity, directory, { allow_partial: true }) : null;
  let names = desktop?.asset_names ?? components.asset_names;
  const tag = `v${identity.app_version}`;
  await verifyReleaseTag(gh, repository, tag, identity.git_revision, true);
  let release: Release | undefined = await findRelease(gh, repository, tag);
  if (mode === "desktop" && (!release || release.draft)) throw new Error("Component release must be published before desktop assets are appended");
  const temp = await mkdtemp(join(tmpdir(), "ctl-component-release-"));
  const payload = join(temp, "release.json");
  const component_notes = ["<!-- ctmux-ci:start -->", `Built from commit \`${identity.git_revision}\`.`, `Bundle ID: \`${identity.bundle_id}\`.`, "",
    "Contains verified Linux and macOS component bundles for Intel/AMD and ARM64.",
    ...(names.includes(windowsAgentManifestName) ? ["Contains verified Windows x64 component bundles."] : []),
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
    const canonical_manifest = assets.find((asset) => asset.name === "bundle-set.json");
    if (!release.draft && canonical_manifest) {
      // Tar metadata and signing can change on a fresh build. The first
      // published set is canonical for this tag, so reuse its verified bytes
      // instead of replacing them or making a harmless retry fail.
      const canonical = join(temp, "canonical");
      await restoreCanonicalAssets(repository, identity, directory, mode, assets, canonical, gh);
      if (mode === "desktop") desktop = await validateReleaseBundle(identity, directory, { allow_partial: true });
      names = desktop?.asset_names ?? (await validatedComponentSet(directory, identity)).asset_names;
    } else if (!release.draft && mode === "desktop") {
      throw new Error("Published component release has no canonical bundle-set.json; publish components before appending assets");
    }
    const matches = new Set<string>();
    const local = new Map<string, { digest: string; size: number }>();
    for (const name of names) local.set(name, { digest: await fileDigest(join(directory, name)), size: (await lstat(join(directory, name))).size });
    if (!release.draft && canonical_manifest && !assets.some((asset) => asset.name === "bundle-set.json" && asset.label?.startsWith(managedLabelPrefix) &&
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
    if (release.immutable && names.some((name) => !matches.has(name))) throw new Error("GitHub release immutability prevents appending assets to this release; no release policy was changed");
    // Upload payloads before their manifests so interrupted uploads remain
    // recoverable without advertising an incomplete target or component set.
    const upload_rank = (name: string): number => !name.endsWith(".json") ? 0 : name.startsWith("ctl-cli-") || name === windowsAgentManifestName ? 2 : 1;
    const upload_names = [...names].sort((left, right) => upload_rank(left) - upload_rank(right));
    for (const name of upload_names) {
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
    const appended_windows = names.includes(windowsAgentManifestName) && !assets.some((asset) => asset.name === windowsAgentManifestName);
    if (release.draft || ((desktop || !canonical_manifest || appended_windows) && notes(latest.body) !== latest.body)) {
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
