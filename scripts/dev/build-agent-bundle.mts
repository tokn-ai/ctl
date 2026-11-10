import { spawn, execFile, fork } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs, promisify } from "node:util";
import { addAgentPayloadBytes, agentCargoPackages, agentComponents, readAgentFile, validateBundleProtocols, type AgentComponentMap } from "../shared/agent-bundle.mts";
import { parseComponent, type ComponentInfo } from "../shared/protocol-contract.mts";
import { isLinuxBundleTarget, readElfComponent, type LinuxBundleTarget } from "../shared/elf-component.mts";
import { packageUpdater } from "../ci/package-updater.mts";
import { type UpdaterManifest } from "../shared/updater-bundle.mts";

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
export function localManifest(components: Record<string, ComponentInfo>, files: LocalBundleManifest["files"], target: LinuxBundleTarget = targetTriple): LocalBundleManifest {
  const ordered = {} as AgentComponentMap;
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
    throw new Error("The components were built from different working trees");
  }
  validateBundleProtocols(ordered, agentComponents);
  const manifest: LocalBundleManifest = {
    schema_version: 2, bundle_id: "", target_triple: target, source: "local", components: ordered, files: orderedFiles,
  };
  manifest.bundle_id = createHash("sha256").update(JSON.stringify(manifest)).digest("hex");
  return manifest;
}

/** Package a snapshot of each executable using the metadata compiled into it. */
export async function packageLocalBundle(binary_directory: string, output_directory: string, run?: Runner,
  target: LinuxBundleTarget = targetTriple, signal?: AbortSignal): Promise<LocalBundleManifest> {
  await mkdir(output_directory, { recursive: true, mode: 0o700 });
  const components: Record<string, ComponentInfo> = {};
  const files: LocalBundleManifest["files"] = {};
  let payloadBytes = 0;
  for (const name of agentComponents) {
    signal?.throwIfAborted();
    const bytes = await readAgentFile(join(binary_directory, name), 128 * 1024 * 1024);
    payloadBytes = addAgentPayloadBytes(payloadBytes, bytes.length);
    const destination = join(output_directory, name);
    await writeFile(destination, bytes, { mode: 0o700 });
    await chmod(destination, 0o700);
    components[name] = run ? parseComponent(JSON.parse(await run(destination, ["--component-info"]))) : readElfComponent(bytes, target, name);
    files[name] = { sha256: createHash("sha256").update(bytes).digest("hex"), executable: true };
  }
  const manifest = localManifest(components, files, target);
  const manifest_bytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
  addAgentPayloadBytes(payloadBytes, manifest_bytes.length);
  await writeFile(join(output_directory, "bundle.json"), manifest_bytes, { mode: 0o600 });
  await chmod(join(output_directory, "bundle.json"), 0o600);
  return manifest;
}

async function packageBuiltArtifacts(binary_directory: string, bundle_directory: string, updater_directory: string,
  target: LinuxBundleTarget, signal: AbortSignal): Promise<UpdaterManifest> {
  const manifest = await packageLocalBundle(binary_directory, bundle_directory, undefined, target, signal);
  const build = manifest.components["ctl-agent"].build;
  return packageUpdater({ target, app_version: build.version,
    bundle_id: `${build.version}-dev.${build.source_revision!.slice(0, 12)}`, git_revision: build.source_revision!,
    binary_directory, output_directory: updater_directory, allow_dirty: true, strip: false,
  }, async (path) => {
    signal.throwIfAborted();
    const info = readElfComponent(await readAgentFile(path, 128 * 1024 * 1024), target, "ctlup");
    if (JSON.stringify(info.build) !== JSON.stringify(build)) {
      throw new Error("ctlup and the runtime components were built from different working trees");
    }
    return { stdout: JSON.stringify(info), stderr: "" };
  });
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

const packageArguments = agentCargoPackages.flatMap((name) => ["-p", name]);
const buildExecutables = [...agentComponents, "ctlup"];

async function containerBuild(target: LinuxBundleTarget): Promise<void> {
  const cancellation = new AbortController();
  const stopLease = watchBuildLease(() => readFile("/result/lease", "utf8"), () => cancellation.abort());
  try {
    await runProcess("cargo", ["build", "--locked", "--release", "--target", target, ...packageArguments], {
      cwd: "/workspace", group: true, signal: cancellation.signal,
    });
    // Rootless engines cannot chmod host-owned bind-mount files. Transfer only
    // their bytes; the host retains ownership and applies private store modes.
    for (const name of buildExecutables) {
      cancellation.signal.throwIfAborted();
      const bytes = await readAgentFile(join("/build", target, "release", name), 128 * 1024 * 1024);
      await writeFile(join("/result/binaries", name), bytes);
    }
  } finally {
    stopLease();
  }
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

async function hasCrossCompiler(root: string): Promise<boolean> {
  try {
    await execute("zig", ["version"], { timeout: 10_000 });
    await execute("cargo", ["zigbuild", "--help"], { cwd: root, timeout: 10_000 });
    await execute("rustup", ["target", "list", "--installed"], { cwd: root, timeout: 10_000 });
    await execute("make", ["--version"], { timeout: 10_000 });
    await execute("perl", ["-MFindBin", "-MIPC::Cmd", "-e", "1"], { timeout: 10_000 });
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

async function compileWithZig(root: string, target: LinuxBundleTarget, signal: AbortSignal): Promise<string> {
  const native_target = join(root, "target/development-bundle-build");
  const installed = (await execute("rustup", ["target", "list", "--installed"], { cwd: root })).stdout;
  if (!installed.split(/\s+/u).includes(target)) {
    console.error(`ctl: Installing the Rust target ${target}…`);
    await runOwnedProcess("rustup", ["target", "add", target], signal, { cwd: root });
  }
  console.error(`ctl: Cross-compiling ${target} with Zig…`);
  await runOwnedProcess("cargo", ["zigbuild", "--locked", "--release", "--target", target, "--target-dir", native_target,
    ...packageArguments], signal, { cwd: root, env: { ...process.env, CARGO_INCREMENTAL: "1" } });
  return join(native_target, target, "release");
}

async function compileInContainer(root: string, temporary: string, target: LinuxBundleTarget, signal: AbortSignal): Promise<string> {
  console.error("ctl: Preparing the container compiler (host compilation requires Zig, cargo-zigbuild, rustup, Make, and Perl)…");
  const engine = await selectEngine();
  const arch = target.startsWith("x86_64") ? "amd64" : "arm64";
  const platform = `linux/${arch}`;
  const image = `${imageName}-${arch}`;
  await runOwnedProcess(engine, ["build", "--platform", platform, "--build-arg", `CTL_BUILD_TARGET=${target}`, "--tag", image,
    join(root, "scripts/dev/linux-bundle-builder")], signal);

  const binaries = join(temporary, "binaries");
  // The container may have a different uid. The parent is private and all
  // transferred files remain owned by the host user.
  await chmod(temporary, 0o777);
  await mkdir(binaries);
  await chmod(binaries, 0o777);
  for (const name of buildExecutables) {
    const path = join(binaries, name);
    await writeFile(path, "");
    await chmod(path, 0o666);
  }
  const git = async (args: string[]): Promise<string> => (await execute("git", ["-C", root, ...args])).stdout.trim();
  const common = await git(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
  const gitDirectory = await git(["rev-parse", "--absolute-git-dir"]);
  const gitRelative = relative(common, gitDirectory);
  if (gitRelative.startsWith("..") || isAbsolute(gitRelative)) throw new Error("Worktree Git directory is outside its common directory");
  const cache = createHash("sha256").update(root).digest("hex").slice(0, 16);
  const cidfile = join(temporary, "container-id");
  const removeContainer = async (): Promise<void> => {
    try {
      const id = (await readFile(cidfile, "utf8")).trim();
      if (/^[a-f0-9]{12,64}$/.test(id)) await execute(engine, ["rm", "--force", id], { timeout: 15_000 });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT" && !signal.aborted) {
        console.error("ctl: Container cleanup did not complete; its build lease will expire.");
      }
    }
  };
  let sequence = 0;
  await writeFile(join(temporary, "lease"), String(sequence++));
  const heartbeat = setInterval(() => {
    void writeFile(join(temporary, "lease"), String(sequence++)).catch(() => clearInterval(heartbeat));
  }, 1_000);
  const abort = (): void => { void removeContainer(); };
  signal.addEventListener("abort", abort, { once: true });
  try {
    console.error(`ctl: Compiling ${target} in the Linux container…`);
    await runOwnedProcess(engine, [
      "run", "--rm", "--platform", platform, "--cidfile", cidfile,
      "--volume", `${root}:/workspace:ro`, "--volume", `${common}:/git:ro`,
      "--volume", `${temporary}:/result:rw`, "--volume", `ctl-bundle-target-${cache}-${arch}:/build`,
      "--volume", "ctl-bundle-registry:/usr/local/cargo/registry", "--volume", "ctl-bundle-git:/usr/local/cargo/git",
      "--env", "CARGO_TARGET_DIR=/build", "--env", "CARGO_INCREMENTAL=1", "--env", "GIT_WORK_TREE=/workspace",
      "--env", `GIT_DIR=/git${gitRelative ? `/${gitRelative.split("\\").join("/")}` : ""}`,
      "--env", "GIT_CONFIG_COUNT=1", "--env", "GIT_CONFIG_KEY_0=safe.directory", "--env", "GIT_CONFIG_VALUE_0=/workspace",
      image, "node", "/workspace/scripts/dev/build-agent-bundle.mts", "--container", target,
    ], signal);
    return binaries;
  } catch (error) {
    await removeContainer();
    throw error;
  } finally {
    clearInterval(heartbeat);
    signal.removeEventListener("abort", abort);
  }
}

export async function buildAgentBundle(repository: string, target: LinuxBundleTarget = targetTriple, output_root?: string): Promise<string> {
  const root = resolve(repository);
  const default_root = join(root, "target/development-bundles");
  const outputRoot = output_root ? resolve(root, output_root) : default_root;
  const output = join(outputRoot, target);
  const cancellation = new AbortController();
  const stop = (): void => cancellation.abort();
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  const lifeline = process.env.CTLUP_BUNDLE_BUILD_LIFELINE === "1" ? process.stdin : undefined;
  lifeline?.once("end", stop);
  lifeline?.once("error", stop);
  lifeline?.resume();
  let unlock: (() => Promise<void>) | undefined;
  let staging: string | undefined;
  let container_stage: string | undefined;
  try {
    for (const directory of new Set([default_root, outputRoot])) {
      await mkdir(directory, { recursive: true, mode: 0o700 });
      const info = await lstat(directory);
      if (!info.isDirectory() || (process.getuid && ((info.mode & 0o077) !== 0 || info.uid !== process.getuid()))) {
        throw new Error("The development bundle output must be a private directory owned by the current user");
      }
    }
    console.error(`ctl: Preparing the ${target} build toolchain…`);
    unlock = await acquireLock(join(default_root, `.${target}.lock`), cancellation.signal);
    staging = await mkdtemp(join(outputRoot, ".linux-build-"));
    let binaries: string;
    if (await hasCrossCompiler(root)) {
      binaries = await compileWithZig(root, target, cancellation.signal);
    } else {
      container_stage = await mkdtemp(join(default_root, ".linux-container-"));
      binaries = await compileInContainer(root, container_stage, target, cancellation.signal);
    }
    cancellation.signal.throwIfAborted();
    console.error(`ctl: Packaging and verifying the ${target} executables…`);
    const bundle_stage = join(staging, "bundle");
    const updater_stage = join(staging, "updater");
    await mkdir(updater_stage, { mode: 0o700 });
    await packageBuiltArtifacts(binaries, bundle_stage, updater_stage, target, cancellation.signal);
    const updater_root = join(outputRoot, "updaters");
    await mkdir(updater_root, { recursive: true, mode: 0o700 });
    if (!(await lstat(updater_root)).isDirectory()) throw new Error("development updater output must be a directory");
    cancellation.signal.throwIfAborted();
    await publishLocalBundle(bundle_stage, output);
    await publishLocalBundle(updater_stage, join(updater_root, target));
    console.log(output);
    return output;
  } finally {
    try {
      if (staging) await rm(staging, { recursive: true, force: true });
      if (container_stage) await rm(container_stage, { recursive: true, force: true });
    } finally {
      try {
        await unlock?.();
      } finally {
        process.removeListener("SIGINT", stop);
        process.removeListener("SIGTERM", stop);
        lifeline?.removeListener("end", stop);
        lifeline?.removeListener("error", stop);
        lifeline?.pause();
      }
    }
  }
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args[0] === "--owned-process" && args[1]) return ownedProcess(args[1], args.slice(2));
  if (args.length === 2 && args[0] === "--container" && isLinuxBundleTarget(args[1])) return containerBuild(args[1]);
  const { values } = parseArgs({ args, options: { target: { type: "string" }, "output-root": { type: "string" }, help: { type: "boolean" } } });
  if (values.help) {
    console.log("usage: pnpm bundles:build [--target x86_64-unknown-linux-musl|aarch64-unknown-linux-musl] [--output-root DIRECTORY]\nBuild the current working tree's Linux components and ctlup.\nUses host cargo-zigbuild when available, otherwise Docker or Podman.\nOutput: target/development-bundles/<target>/");
    return;
  }
  const target = values.target ?? targetTriple;
  if (!isLinuxBundleTarget(target)) throw new Error("Choose the x86_64-unknown-linux-musl or aarch64-unknown-linux-musl target");
  if (values["output-root"] === "") throw new Error("The output root must name a directory");
  await buildAgentBundle(resolve(dirname(fileURLToPath(import.meta.url)), "../.."), target, values["output-root"]);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(`Local Linux bundle build failed: ${error instanceof Error ? error.message : String(error)}`); process.exitCode = 1; });
}
