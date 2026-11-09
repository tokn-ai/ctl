import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, readdir, realpath, rename, writeFile } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { agentCargoPackages, agentComponents, validateBundleProtocols, type AgentComponentMap } from "../shared/agent-bundle.mts";
import { maxComponentMetadata, parseComponent, type ComponentInfo } from "../shared/protocol-contract.mts";
import { selectDaemonBuildArguments } from "./daemon-build.mts";
import { metadataArguments, targetFromArtifacts } from "./signed-cargo-target.mts";
import { developmentEnvironment, signDevelopmentHelper } from "./ctld-signed.mts";
import { runMacosCommand, type MacosCommandRunner } from "./macos-provisioning.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

const executables = [...agentComponents, "ctlup"] as const;
type Executable = typeof executables[number];
const max_raw_binary_bytes = 512 * 1024 * 1024;
const max_binary_bytes = 128 * 1024 * 1024;

export interface LocalDevelopmentOptions {
  workspace: string;
  output: string;
  cargo_args: string[];
  cargo_cwd?: string;
  native_target?: string;
  env?: NodeJS.ProcessEnv;
}

export interface LocalDevelopmentResult {
  schema_version: 1;
  target_triple: string;
  binary_directory: string;
  ctld_package: string | null;
  manager_executable: string;
}

/** Cargo owns path/config resolution; only the final private cache is overridden. */
function buildArguments(cargo_args: string[], cache: string): string[] {
  const selected = selectDaemonBuildArguments(cargo_args);
  const args = ["build", ...(selected.includes("--locked") || selected.includes("--frozen") ? [] : ["--locked"])];
  for (let index = 0; index < selected.length; index += 1) {
    const argument = selected[index]!;
    if (argument === "--target-dir") { index += 1; continue; }
    if (argument.startsWith("--target-dir=")) continue;
    args.push(argument);
  }
  args.push("--target-dir", cache, "--message-format=json-render-diagnostics", ...agentCargoPackages.flatMap((name) => ["-p", name]));
  return args;
}

function artifacts(stdout: string, cwd: string): Record<Executable, string> {
  const result: Partial<Record<Executable, string>> = {};
  for (const line of stdout.split("\n")) {
    let message;
    try { message = JSON.parse(line); } catch { continue; }
    const name = message?.target?.name;
    if (message?.reason !== "compiler-artifact" || !executables.includes(name) ||
      !message.target.kind?.includes("bin") || message.profile?.test === true || typeof message.executable !== "string") continue;
    const path = resolve(cwd, message.executable);
    const previous = result[name as Executable];
    if (previous !== undefined && previous !== path) throw new Error(`Cargo reported multiple development executables for ${name}`);
    result[name as Executable] = path;
  }
  for (const name of executables) if (!result[name]) throw new Error(`Cargo did not report a development executable for ${name}`);
  return result as Record<Executable, string>;
}

function verifyCohort(components: Record<Executable, ComponentInfo>, expected_version: string): void {
  const first = components["ctl-agent"].build;
  if (first.version !== expected_version || first.source_revision === null || !/^[a-fA-F0-9]{40}$/.test(first.source_revision)) {
    throw new Error("development components lack a valid common source build");
  }
  for (const name of executables) {
    const build = components[name].build;
    if (build.version !== first.version || build.source_revision !== first.source_revision ||
      build.source_fingerprint !== first.source_fingerprint || build.dirty !== first.dirty) {
      throw new Error(`development executable ${name} was built from a different working tree`);
    }
  }
  validateBundleProtocols(components as AgentComponentMap, agentComponents);
}

/** Build and sign an immutable input snapshot. Activation belongs only to ctlup. */
export async function buildLocalDevelopment(
  options: LocalDevelopmentOptions, run: MacosCommandRunner = runMacosCommand,
): Promise<LocalDevelopmentResult> {
  const workspace = await realpath(options.workspace);
  const cargo_cwd = await realpath(options.cargo_cwd ?? process.cwd());
  const env = developmentEnvironment(options.env ?? process.env);
  const invoke = (command: string, args: string[], timeout_ms?: number) => run(command, args, {
    cwd: cargo_cwd, env, timeout_ms, stream_stderr: command === "cargo" && args[0] === "build",
  });
  const metadata = JSON.parse((await invoke("cargo", metadataArguments(options.cargo_args), 30_000)).stdout) as {
    workspace_root?: unknown; target_directory?: unknown; packages?: { name: string; version: string }[];
  };
  if (typeof metadata.workspace_root !== "string" || await realpath(metadata.workspace_root) !== workspace ||
    typeof metadata.target_directory !== "string" || !isAbsolute(metadata.target_directory)) {
    throw new Error("Cargo metadata must describe the requested workspace and an absolute target directory");
  }
  const expected_version = metadata.packages?.find((pkg) => pkg.name === "ctld")?.version;
  if (typeof expected_version !== "string") throw new Error("Cargo metadata did not report the ctld package version");
  const compiler = env.RUSTC ?? env.CARGO_BUILD_RUSTC ?? "rustc";
  const host = /^host: (\S+)$/m.exec((await invoke(compiler, ["-vV"], 10_000)).stdout)?.[1];
  if (!host || !/^(?:x86_64|aarch64)-(?:apple-darwin|unknown-linux-(?:gnu|musl))$/.test(host)) {
    throw new Error("local development installation requires a supported native macOS or Linux compiler");
  }
  const output_metadata = await lstat(options.output);
  if (!output_metadata.isDirectory() || output_metadata.isSymbolicLink() ||
    (process.getuid && output_metadata.uid !== process.getuid()) || (output_metadata.mode & 0o077) !== 0 ||
    (await readdir(options.output)).length !== 0) throw new Error("development output must be an empty owned private directory");
  const output = await realpath(options.output);
  await mkdir(metadata.target_directory, { recursive: true });
  const target_directory = await realpath(metadata.target_directory);
  const checkout_id = createHash("sha256").update(workspace, "utf8").digest("hex").slice(0, 20);
  const development = join(target_directory, "ctl-dev");
  await ensurePrivateDirectory(development);
  const checkout = join(development, checkout_id);
  await ensurePrivateDirectory(checkout);
  const cache = join(checkout, "cargo");
  const compiled = artifacts((await invoke("cargo", buildArguments(options.cargo_args, cache))).stdout, cargo_cwd);
  const normalized = Object.fromEntries(await Promise.all(Object.entries(compiled).map(async ([name, path]) => [name, await realpath(path)]))) as Record<Executable, string>;
  const target = targetFromArtifacts(await realpath(cache), normalized) ?? host;
  const native_target = options.native_target ?? host;
  if (!/^(?:x86_64|aarch64)-(?:apple-darwin|unknown-linux-(?:gnu|musl))$/.test(target) ||
    (target !== native_target && target !== native_target.replace(/-unknown-linux-gnu$/, "-unknown-linux-musl"))) {
    throw new Error("development artifacts do not target an executable native macOS or Linux platform");
  }
  const binary_directory = join(output, "bin");
  await ensurePrivateDirectory(binary_directory);
  // Snapshot every file before probing or stripping any of them. Another Cargo
  // invocation can replace original outputs as soon as this build releases its lock.
  for (const name of executables) {
    const input = compiled[name];
    const info = await lstat(input);
    if (!info.isFile() || info.isSymbolicLink() || info.size === 0 || info.size > max_raw_binary_bytes) throw new Error(`invalid development executable: ${name}`);
    const copied = join(binary_directory, name);
    await copyFile(input, copied);
    await chmod(copied, 0o700);
  }
  await invoke("strip", executables.map((name) => join(binary_directory, name)));
  const components = {} as Record<Executable, ComponentInfo>;
  let total = 0;
  for (const name of executables) {
    const path = join(binary_directory, name);
    const size = (await lstat(path)).size;
    total += size;
    if (size === 0 || size > max_binary_bytes || total > 512 * 1024 * 1024) throw new Error("stripped development executables exceed the installation size limits");
    const { stdout } = await invoke(path, ["--component-info"], 30_000);
    if (Buffer.byteLength(stdout) > maxComponentMetadata) throw new Error(`${name} reported oversized component metadata`);
    components[name] = parseComponent(JSON.parse(stdout));
  }
  verifyCohort(components, expected_version);
  let ctld_package: string | null = null;
  if (target.endsWith("-apple-darwin")) {
    ctld_package = join(output, "helper");
    await signDevelopmentHelper({ repository_root: workspace, target_directory, artifact: join(binary_directory, "ctld"),
      target, app_version: expected_version, output_directory: ctld_package, env }, run);
  }
  const result: LocalDevelopmentResult = { schema_version: 1, target_triple: target, binary_directory,
    ctld_package, manager_executable: join(binary_directory, "ctlup") };
  const stage = join(output, ".result.json");
  await writeFile(stage, `${JSON.stringify(result, null, 2)}\n`, { mode: 0o600, flag: "wx" });
  await rename(stage, join(output, "result.json"));
  return result;
}

async function main(): Promise<void> {
  const owned_group = process.env.CTLUP_DEVELOPMENT_LIFELINE === "1";
  const canceled = () => process.kill(-process.pid, "SIGKILL");
  if (owned_group) {
    process.stdin.once("end", canceled);
    process.stdin.once("error", canceled);
    process.stdin.resume();
  }
  try {
    const values = new Map<string, string>();
    const args = process.argv.slice(2);
    for (let index = 0; index < args.length; index += 2) {
      const flag = args[index]!, value = args[index + 1];
      if (!["--workspace", "--output", "--cargo-args-json", "--cargo-cwd", "--native-target"].includes(flag) || value === undefined || values.has(flag)) {
        throw new Error("usage: build-local-development.mts --workspace PATH --output DIRECTORY --cargo-args-json JSON [--cargo-cwd PATH] [--native-target TARGET]");
      }
      values.set(flag, value);
    }
    const workspace = values.get("--workspace"), output = values.get("--output"), encoded = values.get("--cargo-args-json");
    const cargo_args: unknown = encoded === undefined ? undefined : JSON.parse(encoded);
    if (!workspace || !output || !Array.isArray(cargo_args) || cargo_args.some((arg) => typeof arg !== "string")) throw new Error("workspace, output and a Cargo argument array are required");
    await buildLocalDevelopment({ workspace, output, cargo_args, cargo_cwd: values.get("--cargo-cwd"), native_target: values.get("--native-target") });
  } finally {
    if (owned_group) {
      process.stdin.off("end", canceled);
      process.stdin.off("error", canceled);
      process.stdin.destroy();
    }
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
