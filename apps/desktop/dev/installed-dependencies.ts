import { unwatchFile, watchFile } from "node:fs";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { searchForWorkspaceRoot, type Plugin } from "vite";

async function installedLockfile(path: string): Promise<string | undefined> {
  try {
    return await readFile(path, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
}

/** Keep a persistent frontend on the dependency versions pnpm has installed. */
export function refreshInstalledDependencies(): Plugin {
  return {
    name: "ctmux:refresh-installed-dependencies",
    async configureServer(server) {
      const http_server = server.httpServer;
      if (!http_server) return;
      // Vite hashes this installed lockfile when its optimizer starts. Source
      // manifests can change before pnpm has finished replacing dependencies.
      const lockfile = join(searchForWorkspaceRoot(server.config.root), "node_modules/.pnpm/lock.yaml");
      const contents = await installedLockfile(lockfile);
      let closed = false;
      let pending = Promise.resolve();
      const changed = () => {
        pending = pending.then(async () => {
          if (closed || !http_server.listening) return;
          const next = await installedLockfile(lockfile);
          if (closed || next === undefined || next === contents) return;
          server.config.logger.info("Installed frontend dependencies changed, refreshing Vite...");
          // Vite can log a restart failure and resolve without replacing the
          // server. Retry briefly while an installation finishes settling.
          for (let attempt = 0; attempt < 3; attempt++) {
            await server.restart(true);
            if (closed) return;
            if (attempt < 2) await delay(250);
          }
          server.config.logger.error("Installed frontend dependencies could not be refreshed. Restart the development launcher after resolving the Vite error.");
        }).catch((error: unknown) => {
          server.config.logger.error(`Could not refresh frontend dependencies: ${error instanceof Error ? error.message : String(error)}`);
        });
      };
      // A separate file watcher reaches pnpm's metadata inside node_modules,
      // which Vite's source watcher deliberately ignores.
      const listening = () => {
        watchFile(lockfile, { interval: 250, persistent: false }, changed);
        // Recheck installation changes that arrived while Vite was starting.
        changed();
      };
      http_server.once("listening", listening);
      http_server.once("close", () => {
        closed = true;
        http_server.off("listening", listening);
        unwatchFile(lockfile, changed);
      });
    },
  };
}
