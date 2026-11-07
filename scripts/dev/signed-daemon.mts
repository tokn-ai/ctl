import { spawn, execFile as execFileCallback, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import { lstat, readlink, realpath, rename, rm, symlink } from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { helperProtocol, maxComponentMetadata, negotiateProtocol, parseHelperComponent, sameProtocols, type ProtocolInfo } from "../shared/protocol-contract.mts";
import { inspectDaemon } from "./signed-daemon-probe.mts";
import { withPreparationLock } from "./signed-preparation-lock.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

const execFile = promisify(execFileCallback);

export interface SignedDaemonConfig {
  runtime_directory: string;
}

interface SignedDaemonOperations {
  startup_timeout_ms?: number;
  shutdown_timeout_ms?: number;
  preparation_timeout_ms?: number;
  on_diagnostic?: (message: string) => void;
}

interface OwnedDaemon {
  child: ChildProcess;
  exited: Promise<void>;
  finished: boolean;
  spawn_error?: Error;
}

/** Manage a worktree endpoint using complete installations verified by ctlup. */
export class SignedDaemon {
  readonly executable: string;
  readonly socket_path: string;
  private pending: Promise<void> = Promise.resolve();
  private closing = false;
  private last_diagnostic?: string;
  private readonly config: SignedDaemonConfig;
  private readonly operations: SignedDaemonOperations;

  constructor(config: SignedDaemonConfig, operations: SignedDaemonOperations = {}) {
    this.config = config;
    this.operations = operations;
    this.executable = path.join(config.runtime_directory, "ctld.app/Contents/MacOS/ctld");
    this.socket_path = path.join(config.runtime_directory, "ctld.sock");
  }

  prepare(executable: string): Promise<void> {
    if (this.closing) return Promise.reject(new Error("signed ctld supervisor is closing"));
    const preparation = this.pending.then(async () => {
      await ensurePrivateDirectory(this.config.runtime_directory);
      await withPreparationLock(this.config.runtime_directory, () => this.prepareDaemon(executable), this.operations.preparation_timeout_ms);
    });
    this.pending = preparation.catch(() => {});
    return preparation;
  }

  /** The independently owned daemon and ctlup installation survive launcher exit. */
  close(): Promise<void> {
    this.closing = true;
    return this.pending;
  }

  private async prepareDaemon(candidate: string): Promise<void> {
    const executable = await realpath(candidate);
    const info = await lstat(executable);
    const bundle = path.dirname(path.dirname(path.dirname(executable)));
    if (!path.isAbsolute(candidate) || !info.isFile() || path.basename(executable) !== "ctld" || path.basename(bundle) !== "ctld.app") {
      throw new Error("development ctld must come from the verified ctlup app package");
    }
    const protocols = parseHelperComponent((await execFile(executable, ["--component-info"], {
      env: helperEnvironment(), timeout: 5_000, maxBuffer: maxComponentMetadata,
    })).stdout).protocols;
    const selectionChanged = await this.selectHelper(bundle);
    const runningProtocol = await inspectDaemon(this.socket_path, this.operations.startup_timeout_ms ?? 5_000);
    if (runningProtocol !== undefined) {
      let diagnostic: string | undefined;
      if (!negotiateProtocol(runningProtocol, helperProtocol(protocols))) {
        diagnostic = `Signed development ctld is still using protocol ${runningProtocol.version}; the selected helper uses ${helperProtocol(protocols).version}. Existing connections were preserved. Open About ctmux and explicitly restart ctld to use the new helper.`;
      } else if (selectionChanged) {
        diagnostic = "New signed helper selected; use About → Restart ctld to apply it. Existing connections preserved.";
      }
      if (diagnostic && (selectionChanged || diagnostic !== this.last_diagnostic)) {
        (this.operations.on_diagnostic ?? console.warn)(diagnostic);
      }
      this.last_diagnostic = diagnostic;
      return;
    }
    this.last_diagnostic = undefined;
    const daemon = ownDaemon(spawn(executable, ["--socket", this.socket_path], {
      cwd: "/",
      env: { ...helperEnvironment(), CTLD_BIN: executable, CTLD_RUNTIME_DIR: this.config.runtime_directory, CTLD_SOCKET_PATH: this.socket_path },
      detached: true,
      stdio: "ignore",
    }));
    try {
      await this.waitForDaemon(daemon, protocols);
      daemon.child.unref();
    } catch (error) {
      // A different owner can win the bind. Never unlink or terminate it.
      await stopOwnedDaemon(daemon, this.operations.shutdown_timeout_ms ?? 2_000);
      throw error;
    }
  }

  private async selectHelper(bundle: string): Promise<boolean> {
    const selected = path.join(this.config.runtime_directory, "ctld.app");
    try {
      if (!(await lstat(selected)).isSymbolicLink()) throw new Error("selected signed ctld path is not a symbolic link");
      if (await readlink(selected) === bundle) return false;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    const link = path.join(this.config.runtime_directory, `.selected-${randomUUID()}`);
    try { await symlink(bundle, link); await rename(link, selected); return true; }
    finally { await rm(link, { force: true }); }
  }

  private async waitForDaemon(daemon: OwnedDaemon, expectedProtocols: ProtocolInfo[]): Promise<void> {
    const deadline = Date.now() + (this.operations.startup_timeout_ms ?? 5_000);
    while (Date.now() < deadline) {
      if (!isRunning(daemon)) throw daemon.spawn_error ?? new Error("new signed ctld stopped during startup");
      const version = await inspectDaemon(this.socket_path, Math.max(1, deadline - Date.now()));
      if (version !== undefined) {
        if (!sameProtocols([version], [helperProtocol(expectedProtocols)])) throw new Error("new signed ctld returned an unexpected protocol version");
        return;
      }
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    throw new Error("new signed ctld did not become ready before startup timed out");
  }
}

function helperEnvironment(): NodeJS.ProcessEnv {
  const environment = { ...process.env };
  for (const name of ["CTLD_ASKPASS", "CTLD_ASKPASS_TOKEN", "CTLD_IDENTITY_ASKPASS", "CTLD_IDENTITY_ASKPASS_SOCKET", "CTLD_IDENTITY_ASKPASS_TOKEN"]) delete environment[name];
  return environment;
}

function ownDaemon(child: ChildProcess): OwnedDaemon {
  const daemon: OwnedDaemon = { child, finished: false, exited: Promise.resolve() };
  daemon.exited = new Promise((resolve) => {
    child.once("error", (error) => { daemon.spawn_error = error; });
    child.once("close", () => { daemon.finished = true; resolve(); });
  });
  return daemon;
}

function isRunning(daemon: OwnedDaemon): boolean {
  return !daemon.finished && !daemon.spawn_error && daemon.child.exitCode === null && daemon.child.signalCode === null;
}

async function stopOwnedDaemon(daemon: OwnedDaemon, timeout: number): Promise<void> {
  for (const signal of ["SIGTERM", "SIGKILL"] as const) {
    if (!isRunning(daemon)) return;
    daemon.child.kill(signal);
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([daemon.exited, new Promise<void>((resolve) => { timer = setTimeout(resolve, timeout); })]);
    } finally {
      clearTimeout(timer);
    }
  }
  if (isRunning(daemon)) throw new Error("new signed ctld did not stop after failed startup");
}
