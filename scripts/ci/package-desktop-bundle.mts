import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { chmod, copyFile, lstat, mkdir, readdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const execute = promisify(execFile);
const macTargets = new Set(["x86_64-apple-darwin", "aarch64-apple-darwin"]);
const linuxTargets = new Set(["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]);

export type SigningMode = "signed" | "unsigned" | "not_applicable";

export interface DesktopBundleOptions {
  target: string;
  bundle_id: string;
  git_revision: string;
  signing_mode: SigningMode;
  input_directory: string;
  output_directory: string;
}

export interface DesktopBundleManifest {
  schema_version: 1;
  target: string;
  bundle_id: string;
  git_revision: string;
  signing_mode: SigningMode;
  assets: { name: string; sha256: string }[];
}

async function findInstaller(inputDirectory: string, directory: string, extension: string): Promise<string> {
  const packageDirectory = join(inputDirectory, directory);
  let names: string[];
  try {
    names = await readdir(packageDirectory);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      throw new Error(`missing ${extension} installer in ${packageDirectory}`);
    }
    throw error;
  }
  const matches = names.filter((name) => name.endsWith(extension)).sort();
  if (matches.length !== 1) {
    throw new Error(`expected exactly one ${extension} installer in ${packageDirectory}; found ${matches.length}`);
  }
  const installer = join(packageDirectory, matches[0]);
  const info = await lstat(installer);
  if (!info.isFile() || info.size === 0) {
    throw new Error(`installer must be a nonempty regular file: ${installer}`);
  }
  return installer;
}

async function checksum(path: string): Promise<string> {
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(path)) {
    digest.update(chunk);
  }
  return digest.digest("hex");
}

export async function packageDesktopBundle(options: DesktopBundleOptions): Promise<DesktopBundleManifest> {
  const isMac = macTargets.has(options.target);
  if (!isMac && !linuxTargets.has(options.target)) {
    throw new Error(`unsupported desktop target: ${options.target}`);
  }
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._+-]{0,127}$/.test(options.bundle_id)) {
    throw new Error(`invalid bundle ID: ${options.bundle_id}`);
  }
  if (!/^[a-fA-F0-9]{40}$/.test(options.git_revision)) {
    throw new Error("git revision must contain 40 hexadecimal characters");
  }
  if (isMac ? !["signed", "unsigned"].includes(options.signing_mode) : options.signing_mode !== "not_applicable") {
    throw new Error(`invalid signing mode ${options.signing_mode} for ${options.target}`);
  }

  const formats = isMac
    ? [["dmg", ".dmg"]]
    : [["deb", ".deb"], ["rpm", ".rpm"], ["appimage", ".AppImage"]];
  // Resolve every required format before writing release assets, so missing or
  // ambiguous build output cannot produce a seemingly complete manifest.
  const installers = await Promise.all(formats.map(async ([directory, extension]) => ({
    source: await findInstaller(options.input_directory, directory, extension),
    extension,
  })));
  const appDirectory = join(options.input_directory, "macos", "rmux.app");
  if (isMac) {
    const info = await lstat(appDirectory).catch((error: NodeJS.ErrnoException) => {
      if (error.code === "ENOENT") {
        throw new Error(`missing macOS application bundle: ${appDirectory}`);
      }
      throw error;
    });
    if (!info.isDirectory()) {
      throw new Error(`macOS application bundle must be a directory: ${appDirectory}`);
    }
    const plist = await lstat(join(appDirectory, "Contents", "Info.plist"));
    if (!plist.isFile() || plist.size === 0) {
      throw new Error(`missing application metadata in ${appDirectory}`);
    }
  }

  await mkdir(options.output_directory, { recursive: true });
  if ((await readdir(options.output_directory)).length !== 0) {
    throw new Error(`desktop output directory must be empty: ${options.output_directory}`);
  }
  const prefix = `rmux-${options.bundle_id}-${options.target}`;
  const assets: DesktopBundleManifest["assets"] = [];
  const recordAsset = async (name: string): Promise<void> => {
    const sha256 = await checksum(join(options.output_directory, name));
    await writeFile(join(options.output_directory, `${name}.sha256`), `${sha256}  ${name}\n`);
    assets.push({ name, sha256 });
  };
  for (const { source, extension } of installers) {
    const name = `${prefix}${extension}`;
    const destination = join(options.output_directory, name);
    await copyFile(source, destination);
    await chmod(destination, (await lstat(source)).mode & 0o777);
    await recordAsset(name);
  }
  if (isMac) {
    const name = `${prefix}.app.tar.gz`;
    // Archive before artifact upload: GitHub's artifact transport does not
    // preserve the executable modes and symlinks inside a raw .app directory.
    await execute("tar", [
      "-czf", resolve(options.output_directory, name),
      "-C", resolve(options.input_directory, "macos"), "rmux.app",
    ]);
    await recordAsset(name);
  }

  const manifest: DesktopBundleManifest = {
    schema_version: 1,
    target: options.target,
    bundle_id: options.bundle_id,
    git_revision: options.git_revision,
    signing_mode: options.signing_mode,
    assets,
  };
  await writeFile(
    join(options.output_directory, `desktop-${options.target}.json`),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  return manifest;
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.length !== 6) {
    throw new Error("usage: package-desktop-bundle.mts TARGET BUNDLE_ID GIT_REVISION SIGNING_MODE INPUT_DIRECTORY OUTPUT_DIRECTORY");
  }
  const [target, bundle_id, git_revision, signing_mode, input_directory, output_directory] = args;
  const manifest = await packageDesktopBundle({
    target, bundle_id, git_revision, signing_mode: signing_mode as SigningMode, input_directory, output_directory,
  });
  console.log(`Packaged ${manifest.assets.length} desktop assets for ${target} in ${output_directory}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
