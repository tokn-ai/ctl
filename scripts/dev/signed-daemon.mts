import { spawn, execFile as execFileCallback, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, mkdtemp, readFile, rename, rm, symlink } from "node:fs/promises";
import { createConnection } from "node:net";
import path from "node:path";
import { promisify } from "node:util";

const execFile = promisify(execFileCallback);
const maxFrameSize = 64 * 1024;

interface SignedDaemonConfig {
  runtime_directory: string;
  profile_path: string;
  app_version: string;
  repository_root: string;
}

interface SignedDaemonOperations {
  package_bundle?: (executable: string, bundle: string) => Promise<void>;
  startup_timeout_ms?: number;
  shutdown_timeout_ms?: number;
}

interface OwnedDaemon {
  child: ChildProcess;
  exited: Promise<void>;
  finished: boolean;
  spawn_error?: Error;
}

/** Keeps the signed development helper in step with each native app build. */
export class SignedDaemon {
  readonly executable: string;
  readonly socket_path: string;
  private readonly config: SignedDaemonConfig;
  private readonly operations: SignedDaemonOperations;
  private pending: Promise<void> = Promise.resolve();
  private closing = false;
  private closed?: Promise<void>;
  private daemon?: OwnedDaemon;
  private fingerprint?: string;
  private readonly bundle_directories: string[] = [];

  constructor(config: SignedDaemonConfig, operations: SignedDaemonOperations = {}) {
    this.config = config;
    this.operations = operations;
    this.executable = path.join(config.runtime_directory, "ctld.app/Contents/MacOS/ctld");
    this.socket_path = path.join(config.runtime_directory, "ctld.sock");
  }

  prepare(executable: string): Promise<void> {
    if (this.closing) {
      return Promise.reject(new Error("signed ctld supervisor is closing"));
    }
    const preparation = this.pending.then(() => this.prepareDaemon(executable));
    // One failed signing attempt must not prevent a later rebuild from succeeding.
    this.pending = preparation.catch(() => {});
    return preparation;
  }

  close(): Promise<void> {
    this.closing = true;
    this.closed ??= this.pending.then(async () => {
      await this.stopDaemon();
      await rm(this.socket_path, { force: true });
      await rm(path.join(this.config.runtime_directory, "ctld.app"), { force: true });
      for (const directory of this.bundle_directories) {
        await rm(directory, { recursive: true, force: true });
      }
    });
    return this.closed;
  }

  private async prepareDaemon(executable: string): Promise<void> {
    const hash = createHash("sha256");
    for await (const chunk of createReadStream(executable)) {
      hash.update(chunk);
    }
    hash.update(await readFile(this.config.profile_path));
    hash.update(this.config.app_version);
    const fingerprint = hash.digest("hex");
    if (fingerprint === this.fingerprint && this.daemon && isRunning(this.daemon)) {
      return;
    }

    await mkdir(this.config.runtime_directory, { recursive: true });
    const directory = await mkdtemp(
      path.join(this.config.runtime_directory, `build-${fingerprint.slice(0, 12)}-`),
    );
    this.bundle_directories.push(directory);
    const bundle = path.join(directory, "ctld.app");
    const packagedExecutable = path.join(bundle, "Contents/MacOS/ctld");
    try {
      if (this.operations.package_bundle) {
        await this.operations.package_bundle(executable, bundle);
      } else {
        await packageBundle(executable, bundle, this.config);
      }
      const { stdout } = await execFile(packagedExecutable, ["--protocol-version"], {
        cwd: this.config.repository_root,
        timeout: 5_000,
      });
      const protocolVersion = Number(stdout.trim());
      if (!/^\d+$/.test(stdout.trim()) || !Number.isSafeInteger(protocolVersion) || protocolVersion > 65_535) {
        throw new Error("signed ctld returned an invalid local protocol version");
      }

      // Do not replace a live helper until its replacement has been signed successfully.
      const nextLink = path.join(directory, "current.app");
      await symlink(bundle, nextLink);
      await rename(nextLink, path.join(this.config.runtime_directory, "ctld.app"));
      await this.stopDaemon();
      await rm(this.socket_path, { force: true });
      const daemon = ownDaemon(spawn(packagedExecutable, ["--socket", this.socket_path], {
        cwd: this.config.repository_root,
        env: {
          ...process.env,
          CTLD_BIN: this.executable,
          CTLD_RUNTIME_DIR: this.config.runtime_directory,
          CTLD_SOCKET_PATH: this.socket_path,
        },
        stdio: ["ignore", "inherit", "inherit"],
      }));
      this.daemon = daemon;
      try {
        await waitForHandshake(
          this.socket_path,
          daemon,
          protocolVersion,
          this.operations.startup_timeout_ms ?? 5_000,
        );
      } catch (error) {
        await this.stopDaemon();
        await rm(this.socket_path, { force: true });
        throw error;
      }
      this.fingerprint = fingerprint;
    } catch (error) {
      // Failed bundles are never reused. Other successful builds remain available
      // for SSH askpass children until the development session finishes.
      await rm(directory, { recursive: true, force: true });
      throw error;
    }
  }

  private async stopDaemon(): Promise<void> {
    const daemon = this.daemon;
    if (!daemon) {
      return;
    }
    if (isRunning(daemon)) {
      daemon.child.kill("SIGTERM");
      await waitUntilExit(daemon, this.operations.shutdown_timeout_ms ?? 2_000);
      if (isRunning(daemon)) {
        daemon.child.kill("SIGKILL");
        await waitUntilExit(daemon, this.operations.shutdown_timeout_ms ?? 2_000);
        if (isRunning(daemon)) {
          throw new Error("owned signed ctld did not stop after SIGKILL");
        }
      }
    }
    this.daemon = undefined;
  }
}

async function packageBundle(executable: string, bundle: string, config: SignedDaemonConfig): Promise<void> {
  const child = ownDaemon(spawn(
    path.join(config.repository_root, "scripts/ci/package-ctld-app.sh"),
    [executable, bundle, config.app_version],
    {
      cwd: config.repository_root,
      env: { ...process.env, CTLD_PROVISIONING_PROFILE: config.profile_path },
      stdio: "inherit",
    },
  ));
  await child.exited;
  if (child.spawn_error) {
    throw child.spawn_error;
  }
  if (child.child.exitCode !== 0) {
    throw new Error(`ctld signing failed with ${child.child.signalCode ?? `status ${child.child.exitCode}`}`);
  }
}

function ownDaemon(child: ChildProcess): OwnedDaemon {
  const daemon: OwnedDaemon = { child, finished: false, exited: Promise.resolve() };
  daemon.exited = new Promise((resolve) => {
    child.once("error", (error) => { daemon.spawn_error = error; });
    child.once("close", () => {
      daemon.finished = true;
      resolve();
    });
  });
  return daemon;
}

function isRunning(daemon: OwnedDaemon): boolean {
  return !daemon.finished && !daemon.spawn_error &&
    daemon.child.exitCode === null && daemon.child.signalCode === null;
}

async function waitUntilExit(daemon: OwnedDaemon, timeout: number): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      daemon.exited,
      new Promise<void>((resolve) => { timer = setTimeout(resolve, timeout); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function waitForHandshake(socket: string, daemon: OwnedDaemon, protocolVersion: number, timeout: number): Promise<void> {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (!isRunning(daemon)) {
      throw daemon.spawn_error ?? new Error("signed ctld stopped during startup");
    }
    try {
      await handshake(socket, protocolVersion, Math.max(1, deadline - Date.now()));
      return;
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      if (code !== "ENOENT" && code !== "ECONNREFUSED") {
        throw error;
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error("signed ctld did not accept a local protocol handshake before startup timed out");
}

function handshake(socket: string, protocolVersion: number, timeout: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const connection = createConnection(socket);
    let received = Buffer.alloc(0);
    const finish = (error?: Error): void => {
      connection.destroy();
      if (error) { reject(error); } else { resolve(); }
    };
    connection.setTimeout(timeout, () => {
      finish(new Error("signed ctld local protocol handshake timed out"));
    });
    connection.once("error", finish);
    connection.once("end", () => {
      finish(new Error("signed ctld closed its local protocol handshake"));
    });
    connection.once("connect", () => {
      const payload = Buffer.from(JSON.stringify({ type: "handshake", protocol_version: protocolVersion }));
      const header = Buffer.alloc(4);
      header.writeUInt32BE(payload.length);
      connection.write(Buffer.concat([header, payload]));
    });
    connection.on("data", (chunk) => {
      received = Buffer.concat([received, chunk]);
      if (received.length < 4) { return; }
      const length = received.readUInt32BE();
      if (length > maxFrameSize) {
        finish(new Error("signed ctld returned an oversized handshake frame"));
        return;
      }
      if (received.length < length + 4) { return; }
      try {
        const response = JSON.parse(received.subarray(4, length + 4).toString("utf8")) as {
          type?: string;
          protocol_version?: number;
          message?: string;
        };
        if (response.type !== "handshake_accepted" || response.protocol_version !== protocolVersion) {
          throw new Error(response.message ?? `signed ctld did not accept local protocol ${protocolVersion}`);
        }
        finish();
      } catch (error) {
        finish(error instanceof Error ? error : new Error(String(error)));
      }
    });
  });
}
