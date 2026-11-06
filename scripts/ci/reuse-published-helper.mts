import { execFile } from "node:child_process";
import { copyFile, lstat, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { readVerifiedHelper, type BuildCtlOptions } from "./build-ctl-bundle.mts";
import { fileDigest, repositoryName } from "./bundle-publication.mts";
import { findRelease, managedLabelPrefix, pages, verifyReleaseTag, type GhRunner } from "./update-draft-release.mts";

type HelperIdentity = Pick<BuildCtlOptions, "target" | "app_version" | "git_revision">;
interface Asset { name: string; label: string | null; digest?: string | null; size: number }

/** Keep a retry's embedded CLI helper identical to the first published helper. */
export async function reusePublishedHelper(
  repository: string,
  identity: HelperIdentity,
  directory: string,
  gh: GhRunner,
): Promise<boolean> {
  repositoryName(repository);
  if (!["x86_64-apple-darwin", "aarch64-apple-darwin"].includes(identity.target) ||
    !/^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?(?:\+[a-zA-Z0-9.-]+)?$/.test(identity.app_version) ||
    !/^[a-f0-9]{40}$/.test(identity.git_revision)) {
    throw new Error("Expected a macOS target, release version, and Git revision");
  }
  const tag = `v${identity.app_version}`;
  await verifyReleaseTag(gh, repository, tag, identity.git_revision, true);
  const release = await findRelease(gh, repository, tag);
  if (!release || release.draft) return false;

  const archive = `ctld-${identity.app_version}-${identity.target}.app.tar.gz`;
  const names = [`ctld-${identity.target}.json`, archive, `${archive}.sha256`];
  const assets = (await pages<Asset>(gh, `repos/${repository}/releases/${release.id}/assets?per_page=100`))
    .filter((asset) => names.includes(asset.name));
  if (assets.length === 0) return false;
  if (assets.length !== names.length || names.some((name) => assets.filter((asset) => asset.name === name).length !== 1)) {
    throw new Error(`Published helper for ${identity.target} is incomplete; restore its original artifacts before retrying`);
  }
  for (const asset of assets) {
    if (!asset.label?.startsWith(managedLabelPrefix)) throw new Error(`Refusing to reuse manually managed release asset: ${asset.name}`);
    const limit = asset.name === archive ? 128 * 1024 * 1024 : asset.name.endsWith(".json") ? 16 * 1024 : 1024;
    if (!/^sha256:[a-f0-9]{64}$/.test(asset.digest ?? "") || !Number.isSafeInteger(asset.size) || asset.size <= 0 || asset.size > limit) {
      throw new Error(`Published helper asset has invalid digest or size: ${asset.name}`);
    }
  }

  const temporary = await mkdtemp(join(tmpdir(), "ctl-reuse-helper-"));
  try {
    await gh(["release", "download", tag, "--repo", repository, "--dir", temporary,
      ...names.flatMap((name) => ["--pattern", name])]);
    for (const asset of assets) {
      const info = await lstat(join(temporary, asset.name));
      if (!info.isFile() || info.size !== asset.size || await fileDigest(join(temporary, asset.name)) !== asset.digest) {
        throw new Error(`Published helper asset does not match its immutable digest or size: ${asset.name}`);
      }
    }
    const manifest = await readVerifiedHelper(temporary, identity);
    if ((await readFile(join(temporary, `${archive}.sha256`), "utf8")).trimEnd() !== `${manifest.sha256}  ${archive}`) {
      throw new Error(`Invalid published helper checksum file: ${archive}.sha256`);
    }
    // Every downloaded file is verified before replacing any newly built file.
    for (const name of names) await copyFile(join(temporary, name), join(directory, name));
    return true;
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const [target, app_version, git_revision, directory, ...extra] = process.argv.slice(2);
  if (!directory || extra.length) throw new Error("usage: reuse-published-helper.mts TARGET APP_VERSION GIT_REVISION ASSET_DIRECTORY");
  const run = promisify(execFile);
  const reused = await reusePublishedHelper(process.env.GH_REPO ?? process.env.GITHUB_REPOSITORY ?? "",
    { target, app_version, git_revision }, resolve(directory), async (args) => (await run("gh", args, { maxBuffer: 16 * 1024 * 1024 })).stdout);
  console.log(reused ? `Reused published helper for ${target}` : `No published helper for ${target}; keeping the new build`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
