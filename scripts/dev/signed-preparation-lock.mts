import { randomUUID } from "node:crypto";
import { lstat, mkdir, open, readdir, rmdir, unlink } from "node:fs/promises";
import path from "node:path";

const ownerPattern = /^owner-([1-9][0-9]*)-([0-9a-f-]{36})$/;

/** Serialize staging/startup across launchers without holding a daemon lifetime lease. */
export async function withPreparationLock<T>(
  runtime_directory: string,
  operation: () => Promise<T>,
  timeout_ms = 30_000,
): Promise<T> {
  const directory = path.join(runtime_directory, "prepare.lock");
  const owner = `owner-${process.pid}-${randomUUID()}`;
  const deadline = Date.now() + timeout_ms;
  while (true) {
    try {
      await mkdir(directory, { mode: 0o700 });
      break;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
    }
    let entries: string[];
    try {
      // Validation must not recreate a lock released between mkdir and lstat.
      const metadata = await lstat(directory);
      if (!metadata.isDirectory() || metadata.isSymbolicLink() ||
        (process.getuid && metadata.uid !== process.getuid()) || (metadata.mode & 0o077) !== 0) {
        throw new Error("signed ctld preparation lock is not a private owned directory");
      }
      entries = await readdir(directory);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
      throw error;
    }
    if (entries.length > 1 || entries.some((entry) => !ownerPattern.test(entry))) {
      throw new Error("signed ctld preparation lock is invalid; no daemon was changed");
    }
    const previous = entries[0];
    if (previous) {
      const pid = Number(ownerPattern.exec(previous)![1]);
      if (!Number.isSafeInteger(pid)) throw new Error("signed ctld preparation lock has an invalid owner");
      if (!running(pid)) {
        try {
          // Only the contender that removes this unique owner file may remove
          // the now-empty directory. Others cannot remove a replacement lock.
          await unlink(path.join(directory, previous));
          await rmdir(directory);
          continue;
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
        }
      }
    }
    if (Date.now() >= deadline) {
      throw new Error("signed ctld preparation is busy or its lock is incomplete; no daemon was changed");
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  let recorded = false;
  try {
    const file = await open(path.join(directory, owner), "wx", 0o600);
    recorded = true;
    await file.close();
    return await operation();
  } finally {
    if (recorded) await unlink(path.join(directory, owner));
    await rmdir(directory);
  }
}

function running(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ESRCH") return false;
    throw error;
  }
}
