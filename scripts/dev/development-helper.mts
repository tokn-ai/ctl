import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { chmod, cp, lstat, mkdtemp, readFile, readdir, realpath, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import type { DevelopmentHelperManifest } from "./ctl-signed.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

export interface DevelopmentHelperSelection {
  schema_version: 1;
  repository_root: string;
  target: string;
  directory: string;
}

interface PublicationOptions {
  repository_root: string;
  target_directory: string;
  app: string;
  manifest: DevelopmentHelperManifest;
}

// Match the installer's expanded archive budget; debug symbols may make an
// uncompressed development executable much larger than its 128 MiB archive.
const max_app_bytes = 512 * 1024 * 1024;
const max_receipt_bytes = 16 * 1024;

/** Publish one immutable, verified helper under the caller's preparation lock. */
export async function publishDevelopmentHelper(
  options: PublicationOptions,
  verify: (app: string, manifest: DevelopmentHelperManifest) => Promise<void>,
): Promise<string> {
  const repository_root = await realpath(options.repository_root);
  const identity = createHash("sha256").update(repository_root, "utf8").digest("hex").slice(0, 20);
  const output = join(options.target_directory, "ctl-dev");
  await ensurePrivateDirectory(output);
  const helpers = join(output, "helpers");
  await ensurePrivateDirectory(helpers);
  const directory = join(helpers, identity);
  await ensurePrivateDirectory(directory);
  const build = `build-${options.manifest.sha256}`;
  if (!/^build-[a-f0-9]{64}$/.test(build)) throw new Error("invalid signed development helper archive identity");
  const receipt = `${JSON.stringify(options.manifest, null, 2)}\n`;
  if (Buffer.byteLength(receipt) > max_receipt_bytes) throw new Error("signed development helper receipt exceeds the install limit");
  const published = join(directory, build);
  const app = join(published, "ctld.app");
  const expected_contents = await appFingerprint(options.app, false);
  if (await exists(published)) {
    // A helper can be the executable of a live broker. Never replace its files,
    // including incomplete or damaged entries; fail without changing selection.
    await verifyPublished(published, options.manifest, expected_contents, verify);
  } else {
    const staging = await mkdtemp(join(directory, ".publish-"));
    try {
      const staged_app = join(staging, "ctld.app");
      await cp(options.app, staged_app, { recursive: true, dereference: false, errorOnExist: true, force: false });
      await makePrivate(staged_app);
      await writeFile(join(staging, "ctld-package.json"), receipt, { mode: 0o600 });
      await verifyPublished(staging, options.manifest, expected_contents, verify);
      // All cooperating publishers hold ctl-dev/prepare.lock. Rename exposes
      // the entire verified directory at once and does not mutate prior builds.
      await rename(staging, published);
    } finally {
      await rm(staging, { recursive: true, force: true });
    }
  }
  const selected = join(directory, "selected.json");
  if (await exists(selected)) await privateFile(selected);
  const selection: DevelopmentHelperSelection = {
    schema_version: 1, repository_root, target: options.manifest.target, directory: build,
  };
  const staging = await mkdtemp(join(directory, ".select-"));
  try {
    const file = join(staging, "selected.json");
    await writeFile(file, `${JSON.stringify(selection, null, 2)}\n`, { mode: 0o600 });
    // Publish selection last: any verification or build failure preserves the
    // old checkpoint, while readers always see one complete JSON document.
    await rename(file, selected);
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
  return join(app, "Contents/MacOS/ctld");
}

async function exists(path: string): Promise<boolean> {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    return false;
  }
}

async function privateFile(path: string): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink() ||
    (process.getuid && info.uid !== process.getuid()) || (info.mode & 0o077) !== 0) {
    throw new Error(`signed development helper metadata must be an owned private file: ${path}`);
  }
}

async function verifyPublished(
  directory: string, manifest: DevelopmentHelperManifest, expected_contents: string,
  verify: (app: string, manifest: DevelopmentHelperManifest) => Promise<void>,
): Promise<void> {
  await ensurePrivateDirectory(directory);
  if ((await readdir(directory)).sort().join() !== "ctld-package.json,ctld.app") {
    throw new Error("cached signed development helper directory is incomplete or contains unexpected files");
  }
  const receipt = join(directory, "ctld-package.json");
  await privateFile(receipt);
  if ((await lstat(receipt)).size > max_receipt_bytes ||
    !isDeepStrictEqual(JSON.parse(await readFile(receipt, "utf8")), manifest)) {
    throw new Error("cached signed development helper receipt does not match the signed build");
  }
  const app = join(directory, "ctld.app");
  if (await appFingerprint(app, true) !== expected_contents) {
    throw new Error("cached signed development helper contents do not match the signed build");
  }
  await verify(app, manifest);
}

async function makePrivate(directory: string): Promise<void> {
  const info = await lstat(directory);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error("signed helper contains an invalid directory");
  await chmod(directory, 0o700);
  for (const name of await readdir(directory)) {
    const file = join(directory, name);
    const info = await lstat(file);
    if (info.isDirectory() && !info.isSymbolicLink()) await makePrivate(file);
    else if (info.isFile()) await chmod(file, (info.mode & 0o111) !== 0 ? 0o755 : 0o600);
    else throw new Error("signed helper contains a symbolic link or unsupported file");
  }
}

/** File contents, names, and executable status must match the signed snapshot. */
async function appFingerprint(directory: string, private_contents: boolean): Promise<string> {
  const hash = createHash("sha256");
  let bytes = 0;
  let entries = 0;
  async function visit(path: string, relative: string): Promise<void> {
    const info = await lstat(path);
    if (info.isSymbolicLink() || (process.getuid && info.uid !== process.getuid()) || ++entries > 1024) {
      throw new Error("signed helper contains an unowned, linked, or oversized entry");
    }
    if (info.isDirectory()) {
      if (private_contents && (info.mode & 0o077) !== 0) throw new Error("cached signed helper directories must be private");
      hash.update(JSON.stringify([relative, "directory"]));
      for (const name of (await readdir(path)).sort()) await visit(join(path, name), `${relative}/${name}`);
    } else if (info.isFile()) {
      const executable = (info.mode & 0o111) !== 0;
      if (private_contents && (info.mode & 0o777) !== (executable ? 0o755 : 0o600)) {
        throw new Error("cached signed helper files have invalid permissions");
      }
      bytes += info.size;
      if (bytes > max_app_bytes) throw new Error("signed development helper exceeds the install limit");
      hash.update(JSON.stringify([relative, "file", executable, info.size]));
      let read_bytes = 0;
      for await (const chunk of createReadStream(path)) {
        read_bytes += chunk.length;
        if (read_bytes > info.size) throw new Error("signed development helper changed during verification");
        hash.update(chunk);
      }
      if (read_bytes !== info.size) throw new Error("signed development helper changed during verification");
    } else {
      throw new Error("signed helper contains an unsupported file");
    }
  }
  await visit(directory, "ctld.app");
  return hash.digest("hex");
}
