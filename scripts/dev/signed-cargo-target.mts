import { execFile as execFileCallback } from "node:child_process";
import { realpath } from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { selectDaemonBuildArguments, type DaemonExecutables } from "./daemon-build.mts";

const execFile = promisify(execFileCallback);

/** Let Cargo resolve config files/includes/environment; do not parse TOML again. */
export function metadataArguments(cargo_args: string[]): string[] {
  const selected = selectDaemonBuildArguments(cargo_args);
  const args = ["metadata", "--no-deps", "--format-version", "1"];
  let target_directory: string | undefined;
  for (let index = 0; index < selected.length; index += 1) {
    const argument = selected[index]!;
    const option = argument.split("=", 1)[0]!;
    if (["--config", "--manifest-path", "--target-dir"].includes(option)) {
      const value = argument.includes("=") ? argument.slice(option.length + 1) : selected[++index]!;
      if (option === "--target-dir") target_directory = value;
      else args.push(option, value);
    } else if (["--offline", "--locked", "--frozen"].includes(argument)) {
      args.push(argument);
    } else if (["--target", "--profile", "--jobs", "--color", "--artifact-dir", "--lockfile-path"].includes(option) && !argument.includes("=")) {
      index += 1;
    } else if (argument === "-Z") {
      args.push(argument, selected[++index]!);
    } else if (argument.startsWith("-Z")) {
      args.push(argument);
    }
  }
  // metadata has no --target-dir flag. This final CLI override has the same
  // precedence over Cargo config/environment as the original build option.
  if (target_directory !== undefined) args.push("--config", `build.target-dir=${JSON.stringify(target_directory)}`);
  return args;
}

export function targetFromArtifacts(target_directory: string, artifacts: DaemonExecutables): string | undefined {
  const directories = new Set(Object.values(artifacts).map((executable) => path.dirname(executable)));
  if (directories.size !== 1) throw new Error("Helper artifacts do not share one Cargo target/profile");
  const relative = path.relative(target_directory, [...directories][0]!);
  const components = relative.split(path.sep);
  if (path.isAbsolute(relative) || components.includes("..") || components.some((value) => value.length === 0) || components.length > 2) {
    throw new Error("Helper artifacts are outside Cargo's reported target directory");
  }
  // Cargo's documented build layout is target/[target-triple/]profile/bin.
  // This also handles custom target JSON names and custom profiles.
  return components.length === 2 ? components[0] : undefined;
}

export async function resolveRunnerTarget(cargo_args: string[], artifacts: DaemonExecutables, cwd: string, env: NodeJS.ProcessEnv): Promise<string> {
  const { stdout } = await execFile("cargo", metadataArguments(cargo_args), { cwd, env, timeout: 30_000, maxBuffer: 16 * 1024 * 1024 });
  const metadata = JSON.parse(stdout) as { target_directory?: unknown };
  if (typeof metadata.target_directory !== "string") throw new Error("Cargo did not report its target directory");
  const normalized = Object.fromEntries(await Promise.all(Object.entries(artifacts).map(async ([name, executable]) => [name, await realpath(executable)]))) as DaemonExecutables;
  const target = targetFromArtifacts(await realpath(metadata.target_directory), normalized);
  if (target) return target;
  // No explicit target directory means Cargo built for the compiler's host.
  const compiler = env.RUSTC ?? env.CARGO_BUILD_RUSTC ?? "rustc";
  const version = await execFile(compiler, ["-vV"], { cwd, env, timeout: 10_000, maxBuffer: 64 * 1024 });
  const host = /^host: (\S+)$/m.exec(version.stdout)?.[1];
  if (!host) throw new Error("rustc did not report its host target");
  return host;
}
