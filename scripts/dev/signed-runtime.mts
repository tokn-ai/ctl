import { createHash } from "node:crypto";
import { lstat, mkdir, mkdtemp, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

/** A short, owner-only endpoint shared by signed launches of one worktree. */
export async function prepareSignedRuntime(
  repository_root: string,
  temporary_root = process.platform === "win32" ? tmpdir() : "/tmp",
): Promise<string> {
  const repository = await realpath(repository_root);
  const temporary = await realpath(temporary_root);
  const identity = createHash("sha256").update(repository).digest("hex").slice(0, 20);
  // macOS's per-user TMPDIR is too long for sockaddr_un after appending a
  // stable worktree identity. Use the short system temp directory instead.
  const directory = path.join(temporary, `rmux-ctld-dev-${process.getuid?.() ?? "user"}-${identity}`);
  await ensurePrivateDirectory(directory);
  return directory;
}

export async function ensurePrivateDirectory(directory: string): Promise<void> {
  try {
    await mkdir(directory, { mode: 0o700 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
  }
  const metadata = await lstat(directory);
  if (
    !metadata.isDirectory() || metadata.isSymbolicLink() ||
    (process.getuid && metadata.uid !== process.getuid()) ||
    (process.platform !== "win32" && (metadata.mode & 0o077) !== 0)
  ) {
    throw new Error(`Signed development runtime must be an owner-only directory: ${directory}`);
  }
}

/** Each app launcher owns only its preparation socket, never the shared daemon. */
export async function createSignedSupervisorDirectory(runtime_directory: string): Promise<string> {
  return mkdtemp(path.join(runtime_directory, "launch-"));
}
