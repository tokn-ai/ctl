import { spawn } from "node:child_process";
import { resolve } from "node:path";
import { createInterface } from "node:readline";

const daemonNames = ["ctld", "rmuxd", "taskd"] as const;
type DaemonName = (typeof daemonNames)[number];

export type DaemonExecutables = Record<DaemonName, string>;

export interface DaemonArtifact {
  daemon: DaemonName;
  executable: string;
}

export class DaemonBuildError extends Error {
  readonly exit_code: number | null;
  readonly signal: NodeJS.Signals | null;
  readonly compilation_failed: boolean;

  constructor(exit_code: number | null, signal: NodeJS.Signals | null, compilation_failed: boolean) {
    super(`Cargo daemon build failed with ${signal ? `signal ${signal}` : `status ${exit_code}`}`);
    this.name = "DaemonBuildError";
    this.exit_code = exit_code;
    this.signal = signal;
    this.compilation_failed = exit_code === 101 && compilation_failed;
  }
}

const buildValueOptions = new Set([
  "--artifact-dir",
  "--color",
  "--config",
  "--jobs",
  "--lockfile-path",
  "--manifest-path",
  "--profile",
  "--target",
  "--target-dir",
]);
const buildFlagOptions = new Set([
  "--frozen",
  "--future-incompat-report",
  "--ignore-rust-version",
  "--keep-going",
  "--locked",
  "--offline",
  "--quiet",
  "--release",
  "--timings",
  "--verbose",
]);
const ignoredValueOptions = new Set([
  "--bench",
  "--bin",
  "--example",
  "--exclude",
  "--features",
  "--message-format",
  "--package",
  "--test",
]);

/** Select Cargo build settings without forwarding app targets or features. */
export function selectDaemonBuildArguments(cargo_args: string[]): string[] {
  const selected: string[] = [];
  for (let index = 0; index < cargo_args.length; index += 1) {
    const argument = cargo_args[index]!;
    if (argument === "--") {
      break;
    }
    const option = argument.split("=", 1)[0]!;
    if (buildValueOptions.has(option) || ignoredValueOptions.has(option)) {
      const forwarded = buildValueOptions.has(option);
      if (argument.includes("=")) {
        if (forwarded) {
          selected.push(argument);
        }
      } else {
        const value = cargo_args[index + 1];
        if (value === undefined || value === "--") {
          throw new Error(`Cargo option ${argument} requires a value`);
        }
        index += 1;
        if (forwarded) {
          selected.push(argument, value);
        }
      }
    } else if (
      buildFlagOptions.has(argument) ||
      argument.startsWith("--timings=") ||
      /^-[vqr]+$/.test(argument)
    ) {
      selected.push(argument);
    } else if (/^-[jmZ]/.test(argument)) {
      selected.push(argument);
      if (argument.length === 2) {
        const value = cargo_args[index + 1];
        if (value === undefined || value === "--") {
          throw new Error(`Cargo option ${argument} requires a value`);
        }
        selected.push(value);
        index += 1;
      }
    } else if (argument === "-p" || argument === "-F") {
      if (cargo_args[index + 1] !== undefined && cargo_args[index + 1] !== "--") {
        index += 1;
      }
    }
  }
  return selected;
}

/** Read only executable binary artifacts for the three local daemons. */
export function parseDaemonArtifact(line: string): DaemonArtifact | undefined {
  let message: unknown;
  try {
    message = JSON.parse(line);
  } catch {
    return undefined;
  }
  if (!isRecord(message) || message.reason !== "compiler-artifact") {
    return undefined;
  }
  const target = message.target;
  if (
    !isRecord(target) ||
    !Array.isArray(target.kind) ||
    !target.kind.includes("bin") ||
    !daemonNames.some((name) => name === target.name) ||
    typeof message.executable !== "string" ||
    message.executable.length === 0 ||
    (isRecord(message.profile) && message.profile.test === true)
  ) {
    return undefined;
  }
  return {
    daemon: target.name as DaemonName,
    executable: message.executable,
  };
}

/** Build matching local helpers and use Cargo's actual output paths. */
export async function buildDaemons(
  cargo_args: string[],
  cwd: string,
  env: NodeJS.ProcessEnv,
): Promise<DaemonExecutables> {
  const build_args = selectDaemonBuildArguments(cargo_args);
  const args = [
    "build",
    ...(build_args.includes("--locked") || build_args.includes("--frozen") ? [] : ["--locked"]),
    ...build_args,
    "--message-format=json-render-diagnostics",
    ...daemonNames.flatMap((name) => ["--package", name]),
  ];
  const child = spawn("cargo", args, {
    cwd,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let diagnostic_tail = "";
  let compilation_failed = false;
  child.stderr.on("data", (chunk: Buffer) => {
    process.stderr.write(chunk);
    // Keep enough trailing text to recognize a diagnostic split across chunks.
    diagnostic_tail += chunk.toString("utf8");
    compilation_failed ||= diagnostic_tail.includes("could not compile");
    diagnostic_tail = diagnostic_tail.slice(-64);
  });
  const completed = new Promise<void>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new DaemonBuildError(code, signal, compilation_failed));
      }
    });
  });
  const artifacts: Partial<DaemonExecutables> = {};
  const lines = createInterface({ input: child.stdout });
  const collect = async (): Promise<void> => {
    for await (const line of lines) {
      const artifact = parseDaemonArtifact(line);
      if (artifact) {
        const executable = resolve(cwd, artifact.executable);
        const previous = artifacts[artifact.daemon];
        if (previous !== undefined && previous !== executable) {
          throw new Error(`Cargo produced multiple executables for ${artifact.daemon}`);
        }
        artifacts[artifact.daemon] = executable;
      }
    }
  };
  try {
    await Promise.all([completed, collect()]);
  } catch (error) {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
    }
    await completed.catch(() => {});
    throw error;
  } finally {
    lines.close();
  }
  for (const name of daemonNames) {
    if (!artifacts[name]) {
      throw new Error(`Cargo daemon build did not report an executable for ${name}`);
    }
  }
  return artifacts as DaemonExecutables;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
