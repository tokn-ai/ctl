import {
  spawn,
  type ChildProcess,
} from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  readFile,
  rm,
} from "node:fs/promises";
import path from "node:path";
import { serveAppSupervisor } from "./signed-app-supervisor.mts";
import { SignedDaemon } from "./signed-daemon.mts";
import { createSignedSupervisorDirectory, prepareSignedRuntime } from "./signed-runtime.mts";
import { getCargoTargetDirectory, openProvisioningProject, prepareProvisioningProfile } from "./macos-provisioning.mts";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "../..");
const appDirectory = path.join(repositoryRoot, "apps/desktop");
async function main(): Promise<void> {
  if (process.platform !== "darwin") {
    throw new Error("signed Tauri development is only available on macOS");
  }

  const targetDirectory = await getCargoTargetDirectory(repositoryRoot);
  if (process.argv.slice(2).includes("--provision")) {
    await openProvisioningProject({ repository_root: repositoryRoot, target_directory: targetDirectory });
    return;
  }

  let supervisorDirectory: string | undefined;
  let daemon: SignedDaemon | undefined;
  let supervisor: Awaited<ReturnType<typeof serveAppSupervisor>> | undefined;
  let tauri: ChildProcess | undefined;
  let appExit: { code: number | null; signal: NodeJS.Signals | null } | undefined;
  const stopTauri = () => {
    if (tauri?.pid) {
      try {
        // The launcher owns this process group, including pnpm, Vite and Cargo.
        process.kill(-tauri.pid, "SIGTERM");
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
    }
  };
  try {
    const profile = await prepareProvisioningProfile({
      repository_root: repositoryRoot, target_directory: targetDirectory,
      provision_command: "pnpm provision",
    });
    const tauriConfig = JSON.parse(
      await readFile(path.join(appDirectory, "src-tauri/tauri.conf.json"), "utf8"),
    ) as { version?: string };
    if (!tauriConfig.version) {
      throw new Error("ctmux has no version in src-tauri/tauri.conf.json");
    }

    const runtimeDirectory = await prepareSignedRuntime(repositoryRoot);
    daemon = new SignedDaemon({
      runtime_directory: runtimeDirectory,
      profile_path: profile.path,
      app_version: tauriConfig.version,
      repository_root: repositoryRoot,
    });
    const ownedDaemon = daemon;
    // The app supervisor belongs to this launcher; the daemon endpoint is
    // shared by every launch of this worktree and survives launcher shutdown.
    supervisorDirectory = await createSignedSupervisorDirectory(runtimeDirectory);
    const supervisorSocket = path.join(supervisorDirectory, "app.sock");
    supervisor = await serveAppSupervisor(supervisorSocket, {
      prepare: (executable) => ownedDaemon.prepare(executable),
      on_error: (error) => console.error(error instanceof Error ? error.message : "Signed daemon preparation failed"),
      on_exit: (result) => {
        appExit = result;
        stopTauri();
      },
    });
    const environment = {
      ...process.env,
      CTLD_BIN: daemon.executable,
      CTLD_RUNTIME_DIR: runtimeDirectory,
      CTLD_SOCKET_PATH: daemon.socket_path,
      CTMUX_DEV_APP_SUPERVISOR: supervisorSocket,
    };
    tauri = spawn("pnpm", ["tauri", "dev", ...process.argv.slice(2)], {
      cwd: appDirectory,
      env: environment,
      stdio: "inherit",
      detached: true,
    });
    process.on("SIGINT", stopTauri);
    process.on("SIGTERM", stopTauri);
    const { code, signal } = await waitForExit(tauri);
    if (appExit) {
      if (appExit.code !== 0) {
        throw new Error(`ctmux exited with ${appExit.signal ? `signal ${appExit.signal}` : `status ${appExit.code ?? "unknown"}`}`);
      }
    } else if (code !== 0 && signal === null) {
      throw new Error(`tauri dev exited with status ${code ?? "unknown"}`);
    }
  } finally {
    stopTauri();
    process.off("SIGINT", stopTauri);
    process.off("SIGTERM", stopTauri);
    await supervisor?.close();
    await daemon?.close();
    if (supervisorDirectory) await rm(supervisorDirectory, { recursive: true, force: true });
  }
}

function waitForExit(
  child: ChildProcess,
): Promise<{ code: number | null; signal: NodeJS.Signals | null }> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
