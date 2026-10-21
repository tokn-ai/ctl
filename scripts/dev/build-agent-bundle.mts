import { spawn, execFile, fork } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { access, chmod, copyFile, lstat, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { agentComponents } from "../shared/agent-bundle.mts";
import { maxComponentMetadata, negotiateProtocol, parseComponent, type ComponentInfo } from "../shared/protocol-contract.mts";

const execute = promisify(execFile);
export const targetTriple = "x86_64-unknown-linux-musl";
const imageName = "ctl-linux-bundle-builder:rust-1.99-node-24";
const leaseTimeout = 20_000;
type Runner = (program: string, args: string[]) => Promise<string>;

export interface LocalBundleManifest {
  schema_version: number;
  bundle_id: string;
  target_triple: string;
  source: "local";
  components: Record<string, ComponentInfo>;
  files: Record<string, { sha256: string; executable: boolean }>;
}

/** Keep the exact field/map order used by ctl_core::bundles::Manifest's content identity. */
export function localManifest(components: Record<string, ComponentInfo>, files: LocalBundleManifest["files"]): LocalBundleManifest {
  const ordered: Record<string, ComponentInfo> = {};
  const orderedFiles: LocalBundleManifest["files"] = {};
  for (const name of [...agentComponents].sort()) {
    const component = parseComponent(components[name]);
    const { version, source_revision, source_fingerprint, dirty } = component.build;
    if (!source_revision) throw new Error(`${name} has no source revision; build from a Git checkout`);
    ordered[name] = { build: { version, source_revision, source_fingerprint, dirty }, protocols: component.protocols };
    const file = files[name];
    if (!file || !/^[a-f0-9]{64}$/.test(file.sha256) || !file.executable) throw new Error(`Missing executable ${name}`);
    orderedFiles[name] = { sha256: file.sha256, executable: true };
  }
  const first = JSON.stringify(ordered["ctl-agent"].build);
  if (Object.values(ordered).some((component) => JSON.stringify(component.build) !== first)) {
    throw new Error("The four components were built from different working trees");
  }
  for (const [consumer, provider, protocol] of [
    ["ctl-agent", "ctld", "ctld"], ["ctl-agent", "ctmuxd", "ctmux"], ["ctl-agent", "ctmuxd", "ctmux_control"],
    ["ctl-agent", "ctl-taskd", "task"], ["ctl-agent", "ctl-taskd", "task_control"],
    ["ctl-taskd", "ctmuxd", "ctmux"], ["ctl-taskd", "ctmuxd", "ctmux_control"],
  ]) {
    const client = ordered[consumer].protocols.find((entry) => entry.name === protocol);
    const server = ordered[provider].protocols.find((entry) => entry.name === protocol);
    if (!client || !server || !negotiateProtocol(client, server)) throw new Error(`Components cannot share ${protocol}`);
  }
  const manifest: LocalBundleManifest = {
    schema_version: 1, bundle_id: "", target_triple: targetTriple, source: "local", components: ordered, files: orderedFiles,
  };
  manifest.bundle_id = createHash("sha256").update(JSON.stringify(manifest)).digest("hex");
  return manifest;
}

/** Runs inside Linux, querying the actual copied executables rather than a host build. */
export async function packageLocalBundle(binary_directory: string, output_directory: string, run: Runner = async (program, args) => {
  const { stdout } = await execute(program, args, { timeout: 30_000, maxBuffer: maxComponentMetadata });
  return stdout;
}): Promise<LocalBundleManifest> {
  await mkdir(output_directory, { recursive: true, mode: 0o700 });
  const components: Record<string, ComponentInfo> = {};
  const files: LocalBundleManifest["files"] = {};
  for (const name of agentComponents) {
    const source = join(binary_directory, name);
    const info = await lstat(source);
    if (!info.isFile() || info.size === 0 || info.size > 128 * 1024 * 1024) throw new Error(`Invalid build executable: ${name}`);
    const destination = join(output_directory, name);
    await copyFile(source, destination);
    await chmod(destination, 0o700);
    components[name] = parseComponent(JSON.parse(await run(destination, ["--component-info"])));
    files[name] = { sha256: createHash("sha256").update(await readFile(destination)).digest("hex"), executable: true };
  }
  const manifest = localManifest(components, files);
  await writeFile(join(output_directory, "bundle.json"), `${JSON.stringify(manifest, null, 2)}\n`, { mode: 0o600 });
  await chmod(join(output_directory, "bundle.json"), 0o600);
  return manifest;
}

function runProcess(program: string, args: string[], options: { cwd?: string; env?: NodeJS.ProcessEnv; signal?: AbortSignal; group?: boolean } = {}): Promise<void> {
  return new Promise((accept, reject) => {
    options.signal?.throwIfAborted();
    const child = spawn(program, args, { cwd: options.cwd, env: options.env, stdio: ["ignore", process.stderr, process.stderr], detached: options.group });
    let timer: NodeJS.Timeout | undefined;
    const kill = (signal: NodeJS.Signals): void => {
      if (!child.pid) return;
      if (process.platform === "win32") {
        // Windows has no POSIX process groups. Include rustc/linker children,
        // which otherwise survive termination of the supervising Cargo process.
        execFile("taskkill", ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true }, (error) => {
          if (error) child.kill(signal);
        });
        return;
      }
      try { if (options.group) process.kill(-child.pid, signal); else child.kill(signal); } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
    };
    const abort = (): void => {
      kill("SIGTERM");
      timer = setTimeout(() => kill("SIGKILL"), 3_000);
      timer.unref();
    };
    options.signal?.addEventListener("abort", abort, { once: true });
    child.once("error", reject);
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", abort);
      if (options.signal?.aborted) reject(new Error("Local bundle build cancelled"));
      else if (code !== 0) reject(new Error(`${program} failed (${signal ?? code})`));
      else accept();
    });
  });
}

/** A small IPC supervisor receives disconnect even if the invoking app kills Node with SIGKILL. */
export function runOwnedProcess(program: string, args: string[], signal: AbortSignal, options: { cwd?: string; env?: NodeJS.ProcessEnv } = {}): Promise<void> {
  signal.throwIfAborted();
  return new Promise((accept, reject) => {
    const child = fork(fileURLToPath(import.meta.url), ["--owned-process", program, ...args], {
      execArgv: ["--experimental-strip-types", "--disable-warning=ExperimentalWarning"],
      stdio: ["ignore", process.stderr, process.stderr, "ipc"],
      ...options,
    });
    const abort = (): void => { if (child.connected) child.send("cancel", () => {}); };
    signal.addEventListener("abort", abort, { once: true });
    child.once("error", reject);
    child.once("close", (code) => {
      signal.removeEventListener("abort", abort);
      if (signal.aborted) reject(new Error("Local bundle build cancelled"));
      else if (code !== 0) reject(new Error(`${program} failed (${code})`));
      else accept();
    });
  });
}

async function ownedProcess(program: string, args: string[]): Promise<void> {
  const cancellation = new AbortController();
  const stop = (): void => cancellation.abort();
  process.once("disconnect", stop);
  process.on("message", stop);
  try {
    await runProcess(program, args, { signal: cancellation.signal, group: process.platform !== "win32" });
  } finally {
    process.removeListener("disconnect", stop);
    process.removeListener("message", stop);
    if (process.connected) process.disconnect();
  }
}

/** The lease also handles SIGKILL of the host Node process (e.g. a cancelled Tauri request). */
export function watchBuildLease(read: () => Promise<string>, cancel: () => void, timeout = leaseTimeout, interval = 2_000, now = (): number => performance.now()): () => void {
  let value: string | undefined;
  let changed = now();
  let pending = false;
  const timer = setInterval(() => {
    if (pending) return;
    pending = true;
    void read().then((next) => {
      if (next !== value) { value = next; changed = now(); }
    }).catch(() => {}).finally(() => {
      pending = false;
      if (now() - changed >= timeout) { clearInterval(timer); cancel(); }
    });
  }, interval);
  return () => clearInterval(timer);
}

async function containerBuild(packageOnly: boolean): Promise<void> {
  const cancellation = new AbortController();
  const stopLease = watchBuildLease(() => readFile("/result/lease", "utf8"), () => cancellation.abort());
  const staging = await mkdtemp("/tmp/ctl-bundle-");
  try {
    if (!packageOnly) {
      await runProcess("cargo", ["build", "--locked", "--release", "--target", targetTriple, ...agentComponents.flatMap((name) => ["-p", name])], {
        cwd: "/workspace", group: true, signal: cancellation.signal,
      });
    }
    cancellation.signal.throwIfAborted();
    await packageLocalBundle(`/build/${targetTriple}/release`, staging, async (program, args) => {
      const { stdout } = await execute(program, args, { timeout: 30_000, maxBuffer: maxComponentMetadata, signal: cancellation.signal });
      return stdout;
    });
    // Rootless engines cannot chmod host-owned bind-mount files. Transfer only
    // their bytes; the host retains ownership and applies private store modes.
    for (const name of [...agentComponents, "bundle.json"]) {
      cancellation.signal.throwIfAborted();
      await writeFile(join("/result/bundle", name), await readFile(join(staging, name)));
    }
  } finally { stopLease(); await rm(staging, { recursive: true, force: true }); }
}

async function acquireLock(path: string, signal: AbortSignal): Promise<() => Promise<void>> {
  const token = randomUUID();
  while (true) {
    signal.throwIfAborted();
    try {
      await mkdir(path);
      try {
        await writeFile(join(path, "owner.tmp"), JSON.stringify({ pid: process.pid, token }));
        await rename(join(path, "owner.tmp"), join(path, "owner.json"));
      } catch (error) { await rm(path, { recursive: true, force: true }); throw error; }
      return async () => {
        const owner = JSON.parse(await readFile(join(path, "owner.json"), "utf8"));
        if (owner.token === token) await rm(path, { recursive: true, force: true });
      };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
    }
    try {
      const owner = JSON.parse(await readFile(join(path, "owner.json"), "utf8"));
      try { process.kill(owner.pid, 0); } catch (error) {
        if ((error as NodeJS.ErrnoException).code === "ESRCH") { await rm(path, { recursive: true, force: true }); continue; }
      }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      // A launcher can be killed between mkdir and publishing its owner record.
      try {
        if (Date.now() - (await lstat(path)).mtimeMs > leaseTimeout) await rm(path, { recursive: true, force: true });
      } catch (missing) { if ((missing as NodeJS.ErrnoException).code !== "ENOENT") throw missing; }
    }
    await new Promise((done) => setTimeout(done, 250));
  }
}

async function selectEngine(): Promise<string> {
  const choices = process.env.CTL_CONTAINER_ENGINE ? [process.env.CTL_CONTAINER_ENGINE] : ["docker", "podman"];
  for (const engine of choices) {
    try { await execute(engine, ["info"], { timeout: 15_000, maxBuffer: 1024 * 1024 }); return engine; } catch {}
  }
  throw new Error(`Start a Linux container engine (${choices.join(" or ")}) before building remote components`);
}

async function hasCrossCompiler(): Promise<boolean> {
  try {
    await execute("zig", ["version"], { timeout: 10_000 });
    await execute("cargo", ["zigbuild", "--help"], { timeout: 10_000 });
    return true;
  } catch { return false; }
}

export async function publishLocalBundle(staging: string, destination: string): Promise<void> {
  const previous = `${destination}.previous-${randomUUID()}`;
  let moved = false;
  try { await rename(destination, previous); moved = true; } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
  try { await rename(staging, destination); } catch (error) {
    if (moved) await rename(previous, destination);
    throw error;
  }
  if (moved) await rm(previous, { recursive: true, force: true });
}

export async function buildAgentBundle(repository: string): Promise<string> {
  const root = resolve(repository);
  const outputRoot = join(root, "target/development-bundles");
  const output = join(outputRoot, targetTriple);
  const cancellation = new AbortController();
  const stop = (): void => cancellation.abort();
  process.once("SIGINT", stop); process.once("SIGTERM", stop);
  // Only the user may traverse this parent, including while a bind-mounted
  // staging directory is writable by the container's differently mapped uid.
  await mkdir(outputRoot, { recursive: true, mode: 0o700 });
  await chmod(outputRoot, 0o700);
  const unlock = await acquireLock(join(outputRoot, `.${targetTriple}.lock`), cancellation.signal);
  const temporary = await mkdtemp(join(outputRoot, ".linux-build-"));
  // The container writes only four files and a manifest into this host-owned directory.
  await chmod(temporary, 0o777);
  await mkdir(join(temporary, "bundle"), { mode: 0o777 });
  await chmod(join(temporary, "bundle"), 0o777);
  // Preserve host ownership even when a rootful container overwrites the payload.
  for (const name of [...agentComponents, "bundle.json"]) {
    await writeFile(join(temporary, "bundle", name), "");
    await chmod(join(temporary, "bundle", name), 0o666);
  }
  let heartbeat: NodeJS.Timeout | undefined;
  let engine: string | undefined;
  let containerMayRun = false;
  const cidfile = join(temporary, "container-id");
  const removeContainer = async (): Promise<void> => {
    if (!engine || !containerMayRun) return;
    try {
      const id = (await readFile(cidfile, "utf8")).trim();
      if (/^[a-f0-9]{12,64}$/.test(id)) await execute(engine, ["rm", "--force", id], { timeout: 15_000 });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT" && !cancellation.signal.aborted) {
        console.error("ctl: Container cleanup did not complete; its build lease will expire.");
      }
    }
  };
  try {
    engine = await selectEngine();
    const nativeCross = await hasCrossCompiler();
    const nativeTarget = join(root, "target/development-bundle-build");
    if (nativeCross) {
      console.error("ctl: Cross-compiling the current working tree for Linux x64 with Zig…");
      const installed = (await execute("rustup", ["target", "list", "--installed"], { cwd: root })).stdout;
      if (!installed.split(/\s+/u).includes(targetTriple)) {
        await runOwnedProcess("rustup", ["target", "add", targetTriple], cancellation.signal, { cwd: root });
      }
      await runOwnedProcess("cargo", ["zigbuild", "--locked", "--release", "--target", targetTriple, "--target-dir", nativeTarget,
        ...agentComponents.flatMap((name) => ["-p", name])], cancellation.signal, { cwd: root, env: { ...process.env, CARGO_INCREMENTAL: "1" } });
    } else {
      console.error("ctl: Preparing the Linux x64 container builder (install Zig and cargo-zigbuild for native cross-compilation)…");
      await runOwnedProcess(engine, ["build", "--platform", "linux/amd64", "--tag", imageName, join(root, "scripts/dev/linux-bundle-builder")], cancellation.signal);
    }
    const git = async (args: string[]): Promise<string> => (await execute("git", ["-C", root, ...args])).stdout.trim();
    const common = await git(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    const gitDirectory = await git(["rev-parse", "--absolute-git-dir"]);
    const gitRelative = relative(common, gitDirectory);
    if (gitRelative.startsWith("..") || isAbsolute(gitRelative)) throw new Error("Worktree Git directory is outside its common directory");
    const cache = createHash("sha256").update(root).digest("hex").slice(0, 16);
    let sequence = 0;
    await writeFile(join(temporary, "lease"), String(sequence++));
    heartbeat = setInterval(() => { void writeFile(join(temporary, "lease"), String(sequence++)).catch(stop); }, 1_000);
    console.error(nativeCross ? "ctl: Packaging the Linux x64 executables…" : "ctl: Building and packaging the current working tree for Linux x64…");
    containerMayRun = true;
    const running = runOwnedProcess(engine, [
      "run", "--rm", "--platform", "linux/amd64", "--cidfile", cidfile,
      "--volume", `${root}:/workspace:ro`, "--volume", `${common}:/git:ro`,
      "--volume", `${temporary}:/result:rw`, "--volume", nativeCross ? `${nativeTarget}:/build:ro` : `ctl-bundle-target-${cache}:/build`,
      "--volume", "ctl-bundle-registry:/usr/local/cargo/registry", "--volume", "ctl-bundle-git:/usr/local/cargo/git",
      "--env", "CARGO_TARGET_DIR=/build", "--env", "CARGO_INCREMENTAL=1", "--env", "GIT_WORK_TREE=/workspace",
      "--env", `GIT_DIR=/git${gitRelative ? `/${gitRelative.split("\\").join("/")}` : ""}`,
      "--env", "GIT_CONFIG_COUNT=1", "--env", "GIT_CONFIG_KEY_0=safe.directory", "--env", "GIT_CONFIG_VALUE_0=/workspace",
      nativeCross ? "docker.io/library/node:24-bookworm-slim" : imageName,
      "node", "/workspace/scripts/dev/build-agent-bundle.mts", nativeCross ? "--container-package" : "--container",
    ], cancellation.signal);
    const abort = (): void => { void removeContainer().catch(() => {}); };
    cancellation.signal.addEventListener("abort", abort, { once: true });
    try { await running; containerMayRun = false; } finally { cancellation.signal.removeEventListener("abort", abort); }
    cancellation.signal.throwIfAborted();
    await access(join(temporary, "bundle/bundle.json"));
    for (const name of agentComponents) await chmod(join(temporary, "bundle", name), 0o700);
    await chmod(join(temporary, "bundle/bundle.json"), 0o600);
    await chmod(join(temporary, "bundle"), 0o700);
    await publishLocalBundle(join(temporary, "bundle"), output);
    console.log(output);
    return output;
  } finally {
    clearInterval(heartbeat);
    await removeContainer();
    await rm(temporary, { recursive: true, force: true });
    await unlock();
    process.removeListener("SIGINT", stop); process.removeListener("SIGTERM", stop);
  }
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args[0] === "--owned-process" && args[1]) return ownedProcess(args[1], args.slice(2));
  if (args.length === 1 && ["--container", "--container-package"].includes(args[0])) return containerBuild(args[0] === "--container-package");
  if (args.length === 1 && args[0] === "--help") {
    console.log("usage: pnpm bundles:build\nBuild the current working tree's four Linux x64 components.\nUses native cargo-zigbuild when available, otherwise a Linux container compiler.\nDocker or Podman runs the target binaries for packaging.\nOutput: target/development-bundles/x86_64-unknown-linux-musl/");
    return;
  }
  if (args.length) throw new Error("usage: pnpm bundles:build");
  await buildAgentBundle(resolve(dirname(fileURLToPath(import.meta.url)), "../.."));
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(`Local Linux bundle build failed: ${error instanceof Error ? error.message : String(error)}`); process.exitCode = 1; });
}
