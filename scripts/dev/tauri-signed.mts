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
import { constants } from "node:os";
import { serveAppSupervisor } from "./signed-app-supervisor.mts";
import { SignedDaemon } from "./signed-daemon.mts";
import { createSignedSupervisorDirectory, prepareSignedRuntime } from "./signed-runtime.mts";
import { getCargoTargetDirectory, prepareProvisioningProfile } from "./macos-provisioning.mts";
import { desktopDevArguments } from "./desktop-dev-config.mts";
import { startFrontendProcess } from "./frontend-process.mts";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "../..");
const appDirectory = path.join(repositoryRoot, "apps/desktop");
async function main(): Promise<void> {
  const args = process.argv.slice(2);
  const separator = args.indexOf("--");
  const tauriArgs = separator < 0 ? args : args.slice(0, separator);
  const informational = tauriArgs.some((arg) => ["--help", "-h", "--version", "-V"].includes(arg));
  if (process.platform !== "darwin" || informational) {
    // Use the CLI's Node entry point so Windows needs no shell or .cmd wrapper.
    // Help must also work before a developer has provisioned their Mac.
    const frontend = informational ? undefined : await prepareFrontend(args);
    let tauri: ChildProcess | undefined;
    let stopping = false;
    const interrupt = () => { stopping = true; tauri?.kill("SIGINT"); };
    const terminate = () => { stopping = true; tauri?.kill("SIGTERM"); };
    try {
      tauri = spawn(process.execPath, [
        path.join(appDirectory, "node_modules/@tauri-apps/cli/tauri.js"), "dev", ...(frontend?.args ?? args),
      ], { cwd: appDirectory, stdio: "inherit" });
      process.on("SIGINT", interrupt);
      process.on("SIGTERM", terminate);
      const { code, signal } = await waitForDevelopmentExit(tauri, frontend, () => stopping);
      process.exitCode = code ?? (signal ? 128 + constants.signals[signal] : 1);
    } finally {
      process.off("SIGINT", interrupt);
      process.off("SIGTERM", terminate);
      tauri?.kill("SIGTERM");
      await frontend?.close();
    }
    return;
  }

  const targetDirectory = await getCargoTargetDirectory(repositoryRoot);

  let supervisorDirectory: string | undefined;
  let daemon: SignedDaemon | undefined;
  let supervisor: Awaited<ReturnType<typeof serveAppSupervisor>> | undefined;
  let tauri: ChildProcess | undefined;
  let frontend: Awaited<ReturnType<typeof prepareFrontend>> | undefined;
  let stopping = false;
  let appExit: { code: number | null; signal: NodeJS.Signals | null } | undefined;
  const stopTauri = () => {
    stopping = true;
    if (tauri?.pid) {
      try {
        // The launcher owns this process group, including pnpm and Cargo.
        process.kill(-tauri.pid, "SIGTERM");
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
    }
  };
  try {
    const profile = await prepareProvisioningProfile({
      repository_root: repositoryRoot, target_directory: targetDirectory,
      provision_command: "pnpm ctld:provision",
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
    frontend = await prepareFrontend(args);
    tauri = spawn("pnpm", ["tauri", "dev", ...frontend.args], {
      cwd: appDirectory,
      env: environment,
      stdio: "inherit",
      detached: true,
    });
    process.on("SIGINT", stopTauri);
    process.on("SIGTERM", stopTauri);
    const { code, signal } = await waitForDevelopmentExit(tauri, frontend, () => stopping);
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
    await frontend?.close();
    await supervisor?.close();
    await daemon?.close();
    if (supervisorDirectory) await rm(supervisorDirectory, { recursive: true, force: true });
  }
}

async function prepareFrontend(args: string[]) {
  const config = JSON.parse(
    await readFile(path.join(appDirectory, "src-tauri/tauri.conf.json"), "utf8"),
  ) as Parameters<typeof desktopDevArguments>[1]["config"];
  const frontend = await startFrontendProcess({
    entry_path: path.join(appDirectory, "dev/server-process.mts"),
    app_directory: appDirectory,
  });
  try {
    return {
      ...frontend,
      args: desktopDevArguments(args, { url: frontend.url, config, platform: process.platform }),
    };
  } catch (error) {
    await frontend.close();
    throw error;
  }
}

function waitForDevelopmentExit(
  tauri: ChildProcess,
  frontend: Awaited<ReturnType<typeof prepareFrontend>> | undefined,
  is_stopping: () => boolean,
) {
  const app_exit = waitForExit(tauri);
  if (!frontend) return app_exit;
  return Promise.race([
    app_exit,
    frontend.exited.then(({ code, signal }) => {
      if (is_stopping()) return app_exit;
      throw new Error(`Desktop frontend exited unexpectedly (${signal ?? `status ${code}`})`);
    }),
  ]);
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
