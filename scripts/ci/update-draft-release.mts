import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { appendFile, lstat, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const managedLabelPrefix = "rmux-ci: ";
const notesStart = "<!-- rmux-ci:start -->";
const notesEnd = "<!-- rmux-ci:end -->";
const desktopTargets = [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
] as const;
const remoteTargets = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
] as const;

export interface BundleIdentity {
  app_version: string;
  bundle_id: string;
  git_revision: string;
}

export interface ReleaseBundle extends BundleIdentity {
  asset_directory: string;
  asset_names: string[];
  unsigned_targets: string[];
}

interface ReleaseAsset {
  id: number;
  name: string;
  label: string | null;
}

interface Release {
  id: number;
  tag_name: string;
  draft: boolean;
  body: string | null;
  html_url: string;
}

export interface ReleaseUpdate {
  status: "updated" | "published";
  html_url: string;
}

export type GhRunner = (args: string[]) => Promise<string>;

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function validateIdentity(identity: BundleIdentity): void {
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._+-]*$/.test(identity.app_version)) {
    throw new Error("Invalid app version");
  }
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._+-]{0,127}$/.test(identity.bundle_id)) {
    throw new Error("Invalid bundle ID");
  }
  if (!/^[a-fA-F0-9]{40}$/.test(identity.git_revision)) {
    throw new Error("Git revision must contain 40 hexadecimal characters");
  }
}

function validateName(name: unknown): asserts name is string {
  if (typeof name !== "string" || !/^[a-zA-Z0-9][a-zA-Z0-9._+-]*$/.test(name)) {
    throw new Error(`Unsafe asset filename: ${String(name)}`);
  }
}

async function requireFile(directory: string, name: string): Promise<string> {
  validateName(name);
  const path = join(directory, name);
  if (!(await lstat(path)).isFile()) {
    throw new Error(`Asset must be a regular file: ${name}`);
  }
  return path;
}

async function readJson(directory: string, name: string): Promise<Record<string, unknown>> {
  const value: unknown = JSON.parse(await readFile(await requireFile(directory, name), "utf8"));
  if (!record(value)) {
    throw new Error(`Invalid manifest: ${name}`);
  }
  return value;
}

async function validateAsset(directory: string, name: unknown, sha256: unknown): Promise<string[]> {
  validateName(name);
  if (typeof sha256 !== "string" || !/^[a-f0-9]{64}$/.test(sha256)) {
    throw new Error(`Invalid SHA-256 for ${name}`);
  }
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(await requireFile(directory, name))) {
    hash.update(chunk);
  }
  if (hash.digest("hex") !== sha256) {
    throw new Error(`Checksum mismatch for ${name}`);
  }
  const checksumName = `${name}.sha256`;
  const checksum = await readFile(await requireFile(directory, checksumName), "utf8");
  if (checksum.trimEnd() !== `${sha256}  ${name}`) {
    throw new Error(`Invalid checksum file: ${checksumName}`);
  }
  return [name, checksumName];
}

/** Reject incomplete or mixed builds before making any GitHub changes. */
export async function validateReleaseBundle(
  identity: BundleIdentity,
  asset_directory: string,
): Promise<ReleaseBundle> {
  validateIdentity(identity);
  const directory = resolve(asset_directory);
  const assetNames = new Set<string>();
  const unsignedTargets: string[] = [];
  const addNames = (names: string[]): void => {
    for (const name of names) {
      if (assetNames.has(name)) {
        throw new Error(`Duplicate asset name: ${name}`);
      }
      assetNames.add(name);
    }
  };

  const bundleSet = await readJson(directory, "bundle-set.json");
  if (
    bundleSet.schema_version !== 1 ||
    bundleSet.app_version !== identity.app_version ||
    bundleSet.bundle_id !== identity.bundle_id ||
    bundleSet.git_revision !== identity.git_revision ||
    !record(bundleSet.targets) ||
    Object.keys(bundleSet.targets).length !== remoteTargets.length
  ) {
    throw new Error("Remote bundle set does not match the expected build identity or targets");
  }
  addNames(["bundle-set.json"]);
  for (const target of remoteTargets) {
    const asset = bundleSet.targets[target];
    if (!record(asset) || asset.archive !== `ctl-agent-bundle-${identity.bundle_id}-${target}.tar.gz`) {
      throw new Error(`Invalid remote bundle for ${target}`);
    }
    addNames(await validateAsset(directory, asset.archive, asset.sha256));
  }

  for (const target of desktopTargets) {
    const manifestName = `desktop-${target}.json`;
    const manifest = await readJson(directory, manifestName);
    const extensions = target.endsWith("apple-darwin")
      ? ["dmg", "app.tar.gz"]
      : ["deb", "rpm", "AppImage"];
    const expectedAssets = new Set(extensions.map((extension) => `rmux-${identity.bundle_id}-${target}.${extension}`));
    if (
      manifest.schema_version !== 1 ||
      manifest.target !== target ||
      manifest.bundle_id !== identity.bundle_id ||
      manifest.git_revision !== identity.git_revision ||
      !Array.isArray(manifest.assets)
    ) {
      throw new Error(`Desktop manifest does not match the expected build identity: ${manifestName}`);
    }
    if (manifest.assets.length !== expectedAssets.size) {
      throw new Error(`Desktop manifest must contain every required package format: ${manifestName}`);
    }
    if (target.endsWith("apple-darwin")) {
      if (manifest.signing_mode !== "signed" && manifest.signing_mode !== "unsigned") {
        throw new Error(`Invalid macOS signing mode: ${manifestName}`);
      }
      if (manifest.signing_mode === "unsigned") {
        unsignedTargets.push(target);
      }
    } else if (manifest.signing_mode !== "not_applicable") {
      throw new Error(`Invalid Linux signing mode: ${manifestName}`);
    }
    addNames([manifestName]);
    for (const asset of manifest.assets) {
      if (!record(asset)) {
        throw new Error(`Invalid asset in ${manifestName}`);
      }
      validateName(asset.name);
      if (!expectedAssets.delete(asset.name)) {
        throw new Error(`Unexpected desktop package filename in ${manifestName}: ${asset.name}`);
      }
      addNames(await validateAsset(directory, asset.name, asset.sha256));
    }
  }
  for (const name of await readdir(directory)) {
    if (!assetNames.has(name)) {
      throw new Error(`Unexpected file in release asset directory: ${name}`);
    }
  }
  return {
    ...identity,
    asset_directory: directory,
    asset_names: [...assetNames].sort(),
    unsigned_targets: unsignedTargets,
  };
}

export function releaseNotes(bundle: ReleaseBundle, previous: string | null): string {
  const lines = [
    notesStart,
    `Built from commit \`${bundle.git_revision}\`.`,
    `Bundle ID: \`${bundle.bundle_id}\`.`,
    "",
    "Contains Linux and macOS desktop packages for Intel/AMD and ARM64, plus matching remote-agent bundles.",
    "Each package includes a SHA-256 checksum file. Build manifests record the package hashes and source revision.",
  ];
  if (bundle.unsigned_targets.length > 0) {
    lines.push(
      "",
      `Unsigned macOS builds: ${bundle.unsigned_targets.map((target) => `\`${target}\``).join(", ")}.`,
      "These builds are not Developer ID signed or notarized. Touch ID credential storage is unavailable.",
    );
  }
  lines.push(notesEnd);
  const section = lines.join("\n");
  const body = previous ?? "";
  const start = body.indexOf(notesStart);
  const end = body.indexOf(notesEnd);
  if (start === -1 && end === -1) {
    return body.length > 0 ? `${body}\n\n${section}` : section;
  }
  if (
    start === -1 || end < start ||
    body.indexOf(notesStart, start + notesStart.length) !== -1 ||
    body.indexOf(notesEnd, end + notesEnd.length) !== -1
  ) {
    throw new Error("Release notes contain malformed rmux-ci markers; repair them before retrying");
  }
  return body.slice(0, start) + section + body.slice(end + notesEnd.length);
}

async function pages<T>(gh: GhRunner, endpoint: string): Promise<T[]> {
  const result: unknown = JSON.parse(await gh(["api", endpoint, "--paginate", "--slurp"]));
  if (!Array.isArray(result) || !result.every(Array.isArray)) {
    throw new Error(`Unexpected paginated GitHub response: ${endpoint}`);
  }
  return result.flat() as T[];
}

async function findRelease(gh: GhRunner, repository: string, tag: string): Promise<Release | undefined> {
  const releases = await pages<Release>(gh, `repos/${repository}/releases?per_page=100`);
  const matches = releases.filter((release) => release.tag_name === tag);
  if (matches.length > 1) {
    throw new Error(`Multiple releases use ${tag}; resolve the ambiguity before retrying`);
  }
  return matches[0];
}

/** An existing tag takes precedence over target_commitish, even on a draft. */
async function verifyReleaseTag(gh: GhRunner, repository: string, tag: string, git_revision: string): Promise<void> {
  const refs: unknown = JSON.parse(await gh([
    "api", `repos/${repository}/git/matching-refs/tags/${encodeURIComponent(tag)}`,
  ]));
  if (!Array.isArray(refs)) {
    throw new Error(`Unexpected GitHub tag response for ${tag}`);
  }
  const matches = refs.filter((ref) => record(ref) && ref.ref === `refs/tags/${tag}`);
  if (matches.length === 0) {
    return;
  }
  if (matches.length !== 1) {
    throw new Error(`Multiple Git references match ${tag}`);
  }
  let object: unknown = matches[0].object;
  const visited = new Set<string>();
  for (let depth = 0; depth <= 10; depth += 1) {
    if (!record(object) || typeof object.sha !== "string" || !/^[a-fA-F0-9]{40}$/.test(object.sha)) {
      throw new Error(`Invalid Git object for tag ${tag}`);
    }
    if (object.type === "commit") {
      if (object.sha.toLowerCase() !== git_revision.toLowerCase()) {
        throw new Error(`Tag ${tag} points to ${object.sha}, but the bundles were built from ${git_revision}; refusing to update the draft`);
      }
      return;
    }
    if (object.type !== "tag") {
      throw new Error(`Tag ${tag} does not resolve to a commit`);
    }
    if (visited.has(object.sha) || depth === 10) {
      throw new Error(`Tag ${tag} has a cyclic or excessively nested annotated tag chain`);
    }
    visited.add(object.sha);
    const annotated: unknown = JSON.parse(await gh(["api", `repos/${repository}/git/tags/${object.sha}`]));
    object = record(annotated) ? annotated.object : undefined;
  }
}

/** Only this label permits replacement or removal; manual attachments stay untouched. */
export function planAssetUpdate(existing: ReleaseAsset[], asset_names: string[]): ReleaseAsset[] {
  const names = new Set(asset_names);
  for (const asset of existing) {
    if (names.has(asset.name) && !asset.label?.startsWith(managedLabelPrefix)) {
      throw new Error(`Refusing to overwrite manually managed release asset: ${asset.name}`);
    }
  }
  return existing.filter((asset) => asset.label?.startsWith(managedLabelPrefix) && !names.has(asset.name));
}

export async function updateDraftRelease(
  repository: string,
  bundle: ReleaseBundle,
  gh: GhRunner,
): Promise<ReleaseUpdate> {
  if (!/^[a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+$/.test(repository)) {
    throw new Error("GH_REPO or GITHUB_REPOSITORY must be OWNER/REPO");
  }
  const tag = `v${bundle.app_version}`;
  let release = await findRelease(gh, repository, tag);
  if (release && !release.draft) {
    return { status: "published", html_url: release.html_url };
  }
  await verifyReleaseTag(gh, repository, tag, bundle.git_revision);
  const tempDirectory = await mkdtemp(join(tmpdir(), "rmux-release-"));
  try {
    const notesPath = join(tempDirectory, "notes.md");
    // Validate markers before touching assets, including on an existing draft.
    await writeFile(notesPath, releaseNotes(bundle, release?.body ?? null));
    if (!release) {
      await gh([
        "release", "create", tag, "--repo", repository,
        "--draft", "--target", bundle.git_revision,
        "--title", `rmux ${tag}`, "--notes-file", notesPath,
      ]);
      release = await findRelease(gh, repository, tag);
      if (!release) {
        throw new Error(`Could not find the newly created draft ${tag}`);
      }
    }
    if (!release.draft) {
      return { status: "published", html_url: release.html_url };
    }
    const releaseEndpoint = `repos/${repository}/releases/${release.id}`;
    const assets = await pages<ReleaseAsset>(gh, `${releaseEndpoint}/assets?per_page=100`);
    const obsolete = planAssetUpdate(assets, bundle.asset_names);
    const beforeUpload: Release = JSON.parse(await gh(["api", releaseEndpoint]));
    if (!beforeUpload.draft) {
      return { status: "published", html_url: beforeUpload.html_url };
    }
    // Upload first. A failure must not remove obsolete packages from the last complete build.
    await gh([
      "release", "upload", tag, "--repo", repository, "--clobber",
      ...bundle.asset_names.map((name) => `${join(bundle.asset_directory, name)}#${managedLabelPrefix}${name}`),
    ]);
    const latest: Release = JSON.parse(await gh(["api", releaseEndpoint]));
    if (!latest.draft) {
      return { status: "published", html_url: latest.html_url };
    }
    const payloadPath = join(tempDirectory, "release.json");
    await writeFile(payloadPath, JSON.stringify({
      name: `rmux ${tag}`,
      target_commitish: bundle.git_revision,
      body: releaseNotes(bundle, latest.body),
    }));
    await gh(["api", releaseEndpoint, "--method", "PATCH", "--input", payloadPath]);
    for (const asset of obsolete) {
      await gh(["api", `repos/${repository}/releases/assets/${asset.id}`, "--method", "DELETE"]);
    }
    return { status: "updated", html_url: latest.html_url };
  } finally {
    await rm(tempDirectory, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.length !== 4) {
    throw new Error("usage: update-draft-release.mts APP_VERSION BUNDLE_ID GIT_REVISION ASSET_DIRECTORY");
  }
  const [app_version, bundle_id, git_revision, asset_directory] = args as [string, string, string, string];
  const repository = process.env.GH_REPO ?? process.env.GITHUB_REPOSITORY ?? "";
  const bundle = await validateReleaseBundle({ app_version, bundle_id, git_revision }, asset_directory);
  const result = await updateDraftRelease(repository, bundle, async (gh_args) => {
    const { stdout } = await execFileAsync("gh", gh_args, { maxBuffer: 16 * 1024 * 1024 });
    return stdout;
  });
  console.log(result.status === "published"
    ? `::notice::Release v${app_version} is already published; bump the app version to create the next draft.`
    : `Updated draft release v${app_version} with ${bundle.asset_names.length} assets.`);
  console.log(result.html_url);
  if (process.env.GITHUB_STEP_SUMMARY) {
    await appendFile(process.env.GITHUB_STEP_SUMMARY,
      `\n[${result.status === "updated" ? "Updated draft release" : "Existing published release"} v${app_version}](${result.html_url})\n`,
    );
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
