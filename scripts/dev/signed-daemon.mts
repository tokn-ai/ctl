import { spawn, execFile as execFileCallback, type ChildProcess } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { copyFile, lstat, mkdir, mkdtemp, readFile, readlink, rename, rm, symlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { helperProtocol, maxComponentMetadata, negotiateProtocol, parseHelperComponent, sameProtocols, type ProtocolInfo } from "../shared/protocol-contract.mts";
import { inspectDaemon } from "./signed-daemon-probe.mts";
import { withPreparationLock } from "./signed-preparation-lock.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

const execFile = promisify(execFileCallback);
const recipeFiles = [
  "scripts/ci/package-ctld-app.sh",
  "apps/desktop/src-tauri/macos/ctld/Info.plist",
  "apps/desktop/src-tauri/macos/ctld/Entitlements.plist",
];

export interface SignedDaemonConfig {
  runtime_directory: string;
  profile_path: string;
  app_version: string;
  repository_root: string;
}

interface SignedDaemonOperations {
  package_bundle?: (executable: string, bundle: string) => Promise<void>;
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

interface PreparedHelper {
  bundle: string;
  executable: string;
  protocols: ProtocolInfo[];
}

/** Stages signed helpers while preserving the independently owned development daemon. */
export class SignedDaemon {
  readonly executable: string;
  readonly socket_path: string;
  private readonly config: SignedDaemonConfig;
  private readonly operations: SignedDaemonOperations;
  private pending: Promise<void> = Promise.resolve();
  private closing = false;
  private last_diagnostic?: string;

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

  /** Stop accepting preparations. The daemon and its immutable bundles survive. */
  close(): Promise<void> {
    this.closing = true;
    return this.pending;
  }

  private async prepareDaemon(executable: string): Promise<void> {
    const helper = await this.stageHelper(executable);
    const selectionChanged = await this.selectHelper(helper.bundle);
    const runningProtocol = await inspectDaemon(this.socket_path, this.operations.startup_timeout_ms ?? 5_000);
    if (runningProtocol !== undefined) {
      let diagnostic: string | undefined;
      if (!negotiateProtocol(runningProtocol, helperProtocol(helper.protocols))) {
        diagnostic = `Signed development ctld is still using protocol ${runningProtocol.version}; the selected helper uses ${helperProtocol(helper.protocols).version}. Existing connections were preserved. Open About ctmux and explicitly restart ctld to use the new helper.`;
      } else if (selectionChanged) {
        diagnostic = "New signed helper staged; use About → Restart ctld to apply it. Existing connections preserved.";
      }
      if (diagnostic && (selectionChanged || diagnostic !== this.last_diagnostic)) {
        (this.operations.on_diagnostic ?? console.warn)(diagnostic);
      }
      this.last_diagnostic = diagnostic;
      return;
    }
    this.last_diagnostic = undefined;

    // detached creates a new session. Do not also pass --detach-from-terminal:
    // its second setsid would fail after Node made this child a session leader.
    const daemon = ownDaemon(spawn(helper.executable, ["--socket", this.socket_path], {
      cwd: "/",
      env: {
        ...helperEnvironment(),
        // A retained daemon's proxy children must use its matching helper;
        // only the app follows the selected symlink for an explicit restart.
        CTLD_BIN: helper.executable,
        CTLD_RUNTIME_DIR: this.config.runtime_directory,
        CTLD_SOCKET_PATH: this.socket_path,
      },
      detached: true,
      stdio: "ignore",
    }));
    try {
      await this.waitForDaemon(daemon, helper.protocols);
      daemon.child.unref();
    } catch (error) {
      // Never unlink the endpoint: an explicit external restart might have won
      // the bind. ctld's own shutdown guard removes only its endpoint.
      await stopOwnedDaemon(daemon, this.operations.shutdown_timeout_ms ?? 2_000);
      throw error;
    }
  }

  private async stageHelper(executable: string): Promise<PreparedHelper> {
    const staging = await mkdtemp(path.join(this.config.runtime_directory, ".prepare-"));
    try {
      // Snapshot inputs before hashing/signing, since another Cargo invocation
      // can replace its output or a refreshed profile while preparation runs.
      const artifact = path.join(staging, "artifact");
      const profile = path.join(staging, "profile");
      const recipe = path.join(staging, "recipe");
      await copyFile(executable, artifact);
      await copyFile(this.config.profile_path, profile);
      const hash = createHash("sha256");
      for await (const chunk of createReadStream(artifact)) hash.update(chunk);
      hash.update(await readFile(profile));
      hash.update(this.config.app_version);
      // A persistent bundle cache must also track the signing recipe. Execute
      // these same snapshots so a concurrent edit cannot change what is signed
      // after its cache fingerprint has been calculated.
      for (const relative of recipeFiles) {
        const snapshot = path.join(recipe, relative);
        await mkdir(path.dirname(snapshot), { recursive: true });
        await copyFile(path.join(this.config.repository_root, relative), snapshot);
        hash.update(JSON.stringify([relative, createHash("sha256").update(await readFile(snapshot)).digest("hex")]));
      }
      const fingerprint = hash.digest("hex");
      const directory = path.join(this.config.runtime_directory, `build-${fingerprint}`);
      const bundle = path.join(directory, "ctld.app");
      const packagedExecutable = path.join(bundle, "Contents/MacOS/ctld");
      try {
        await lstat(directory);
        await ensurePrivateDirectory(directory);
        const marker = await readFile(path.join(directory, "fingerprint"), "utf8");
        if (marker !== fingerprint) throw new Error("cached signed ctld bundle metadata is invalid");
        return { bundle, executable: packagedExecutable, protocols: await helperProtocols(packagedExecutable) };
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
        // An incomplete published directory is never overwritten: it might be
        // used by a live process, and a missing marker is not proof otherwise.
        try {
          await lstat(directory);
          throw new Error("cached signed ctld bundle is incomplete; no running daemon was changed");
        } catch (check) {
          if ((check as NodeJS.ErrnoException).code !== "ENOENT") throw check;
        }
      }
      const stagedBundle = path.join(staging, "ctld.app");
      if (this.operations.package_bundle) {
        await this.operations.package_bundle(artifact, stagedBundle);
      } else {
        await packageBundle(artifact, stagedBundle, { ...this.config, profile_path: profile, repository_root: recipe });
      }
      const protocols = await helperProtocols(path.join(stagedBundle, "Contents/MacOS/ctld"));
      await rm(artifact);
      await rm(profile);
      await rm(recipe, { recursive: true });
      await writeFile(path.join(staging, "fingerprint"), fingerprint, { mode: 0o600 });
      await rename(staging, directory);
      return { bundle, executable: packagedExecutable, protocols };
    } finally {
      // Only unpublished snapshots are removed. Published builds can still be
      // executable paths of ctld, SSH askpass, and proxy children across launches.
      await rm(staging, { recursive: true, force: true });
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
    try {
      await symlink(bundle, link);
      await rename(link, selected);
      return true;
    } finally {
      await rm(link, { force: true });
    }
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

async function helperProtocols(executable: string): Promise<ProtocolInfo[]> {
  const { stdout } = await execFile(executable, ["--component-info"], { env: helperEnvironment(), timeout: 5_000, maxBuffer: maxComponentMetadata });
  return parseHelperComponent(stdout).protocols;
}

async function packageBundle(executable: string, bundle: string, config: SignedDaemonConfig): Promise<void> {
  const child = ownDaemon(spawn(path.join(config.repository_root, "scripts/ci/package-ctld-app.sh"), [executable, bundle, config.app_version], {
    cwd: config.repository_root,
    env: {
      ...helperEnvironment(), CTLD_PROVISIONING_PROFILE: config.profile_path,
      CTLD_SIGNING_TIMESTAMP: "none", CTLD_REQUIRE_DISTRIBUTION_SIGNING: "false",
    },
    stdio: "inherit",
  }));
  await child.exited;
  if (child.spawn_error) throw child.spawn_error;
  if (child.child.exitCode !== 0) throw new Error(`ctld signing failed with ${child.child.signalCode ?? `status ${child.child.exitCode}`}`);
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
