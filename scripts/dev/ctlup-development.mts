import { spawn } from "node:child_process";
import { chmod, copyFile, lstat, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { metadataArguments } from "./signed-cargo-target.mts";
import { selectDaemonBuildArguments } from "./daemon-build.mts";

const components = ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld", "ctl", "ctmux", "ctlup"] as const;
const max_output = 16 * 1024 * 1024;
const max_diagnostic = 32 * 1024;

export interface DevelopmentInstallation {
  schema_version: 1;
  bundle_id: string;
  target_triple: string;
  executables: Record<(typeof components)[number], string>;
  services_preserved: true;
}

export interface PreparedDevelopment {
  installation: DevelopmentInstallation;
  exited: Promise<void>;
  close: () => Promise<void>;
}

export class DevelopmentPreparationError extends Error {
  readonly compilation_failed: boolean;
  readonly exit_code: number | null;
  readonly signal: NodeJS.Signals | null;
  constructor(exit_code: number | null, signal: NodeJS.Signals | null, diagnostics: string) {
    super(`ctlup development preparation failed (${signal ?? `status ${exit_code}`}): ${diagnostics.trim()}`);
    this.name = "DevelopmentPreparationError";
    this.exit_code = exit_code;
    this.signal = signal;
    this.compilation_failed = diagnostics.includes("could not compile");
  }
}

/** Bootstrap only the manager. Every component build and activation belongs to ctlup. */
export async function prepareDevelopment(cargo_args: string[], cwd: string, environment: NodeJS.ProcessEnv): Promise<PreparedDevelopment> {
  const env: NodeJS.ProcessEnv = { ...environment, CTL_BUNDLED_CTLD_DIR: undefined, CTL_BUNDLED_CTLD_MODE: undefined };
  const metadata = JSON.parse(await run("cargo", metadataArguments(cargo_args), cwd, env));
  if (typeof metadata.workspace_root !== "string" || !path.isAbsolute(metadata.workspace_root)) {
    throw new Error("Cargo did not report an absolute development workspace");
  }
  const compiler = env.RUSTC ?? env.CARGO_BUILD_RUSTC ?? "rustc";
  const host = /^host: (\S+)$/m.exec(await run(compiler, ["-vV"], cwd, env))?.[1];
  if (!host || !/^[a-zA-Z0-9_.+-]+$/.test(host)) throw new Error("rustc did not report its native target");
  const build_args = withoutTarget(selectDaemonBuildArguments(cargo_args));
  const output = await run("cargo", [
    "build", ...(build_args.includes("--locked") || build_args.includes("--frozen") ? [] : ["--locked"]), ...build_args,
    "--target", host, "--package", "ctl-cli", "--bin", "ctlup", "--message-format=json-render-diagnostics",
  ], cwd, env);
  const artifact = updaterArtifact(output, cwd);
  const temporary = await mkdtemp(path.join(path.dirname(artifact), ".ctlup-bootstrap-"));
  try {
    // Cargo may rebuild ctlup while it prepares the runtime. Execute a private
    // snapshot, and strip only that copy so normal debug outputs stay intact.
    const executable = path.join(temporary, "ctlup");
    await copyFile(artifact, executable);
    await chmod(executable, 0o700);
    await run("strip", [executable], cwd, env);
    const info = await lstat(executable);
    if (!info.isFile() || info.size === 0 || info.size > 128 * 1024 * 1024) throw new Error("invalid bootstrap ctlup executable size");
    const prepared = await startManager(executable, [
      "dev", "--workspace", metadata.workspace_root,
      "--cargo-args-json", JSON.stringify(cargo_args), "--json", "--lease",
    ], cwd, env);
    const close = prepared.close;
    const exited = prepared.exited.finally(() => rm(temporary, { recursive: true, force: true }));
    void exited.catch(() => {});
    return {
      ...prepared,
      close: async () => { try { await close(); } finally { await rm(temporary, { recursive: true, force: true }); } },
      exited,
    };
  } catch (error) {
    await rm(temporary, { recursive: true, force: true });
    throw error;
  }
}

export function developmentEnvironment(installation: DevelopmentInstallation, environment: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  return {
    ...environment,
    CTLD_BIN: installation.executables.ctld,
    CTMUXD_BIN: installation.executables.ctmuxd,
    CTL_TASKD_BIN: installation.executables["ctl-taskd"],
    CTL_DEV_INSTALLATION_ID: installation.bundle_id,
  };
}

export function parseDevelopmentInstallation(value: unknown): DevelopmentInstallation {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("invalid ctlup development response");
  const result = value as DevelopmentInstallation;
  const executables = result.executables;
  if (result.schema_version !== 1 || typeof result.bundle_id !== "string" || !/^[a-f0-9]{64}$/.test(result.bundle_id) ||
    typeof result.target_triple !== "string" || !/^[a-zA-Z0-9_.+-]+$/.test(result.target_triple) || result.services_preserved !== true ||
    !executables || typeof executables !== "object" || Array.isArray(executables) || Object.keys(executables).length !== components.length ||
    components.some((name) => typeof executables[name] !== "string" || !path.isAbsolute(executables[name]))) {
    throw new Error("ctlup did not report a complete native development installation");
  }
  return result;
}

function withoutTarget(args: string[]): string[] {
  const output: string[] = [];
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--target") index += 1;
    else if (!args[index]!.startsWith("--target=")) output.push(args[index]!);
  }
  return output;
}

function updaterArtifact(output: string, cwd: string): string {
  const artifacts = new Set<string>();
  for (const line of output.split("\n")) {
    let value;
    try { value = JSON.parse(line); } catch { continue; }
    if (value?.reason === "compiler-artifact" && value.target?.name === "ctlup" &&
      Array.isArray(value.target.kind) && value.target.kind.includes("bin") && value.profile?.test !== true && typeof value.executable === "string") {
      artifacts.add(path.resolve(cwd, value.executable));
    }
  }
  if (artifacts.size !== 1) throw new Error("Cargo must report exactly one native ctlup executable");
  return [...artifacts][0]!;
}

async function run(program: string, args: string[], cwd: string, env: NodeJS.ProcessEnv): Promise<string> {
  const child = spawn(program, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
  const output: Buffer[] = [];
  let output_size = 0;
  let diagnostics = "";
  let overflow = false;
  child.stdout.on("data", (chunk: Buffer) => {
    output_size += chunk.length;
    if (output_size > max_output) { overflow = true; child.kill("SIGTERM"); }
    else output.push(chunk);
  });
  child.stderr.on("data", (chunk: Buffer) => {
    process.stderr.write(chunk);
    diagnostics = (diagnostics + chunk.toString("utf8")).slice(-max_diagnostic);
  });
  await new Promise<void>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => {
      if (overflow) reject(new Error("development command output exceeds its limit"));
      else if (code === 0) resolve();
      else reject(new DevelopmentPreparationError(code, signal, diagnostics));
    });
  });
  return Buffer.concat(output).toString("utf8");
}

async function startManager(program: string, args: string[], cwd: string, env: NodeJS.ProcessEnv): Promise<PreparedDevelopment> {
  const child = spawn(program, args, { cwd, env, stdio: ["pipe", "pipe", "pipe"] });
  let diagnostics = "";
  child.stderr.on("data", (chunk: Buffer) => {
    process.stderr.write(chunk);
    diagnostics = (diagnostics + chunk.toString("utf8")).slice(-max_diagnostic);
  });
  const closed = new Promise<void>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => code === 0 ? resolve() : reject(new DevelopmentPreparationError(code, signal, diagnostics)));
  });
  // A child can fail before the first line is consumed; observe its rejection
  // immediately while the readiness reader supplies the useful diagnostic.
  void closed.catch(() => {});
  let closing = false;
  let close_pending: Promise<void> | undefined;
  try {
    const installation = await new Promise<DevelopmentInstallation>((resolve, reject) => {
      let bytes = Buffer.alloc(0);
      const fail = (error: unknown) => { child.stdout.off("data", read); reject(error); };
      const read = (chunk: Buffer) => {
        bytes = Buffer.concat([bytes, chunk]);
        if (bytes.length > 64 * 1024) { fail(new Error("ctlup development response exceeds its limit")); return; }
        const newline = bytes.indexOf(10);
        if (newline < 0) return;
        child.stdout.off("data", read);
        child.stdout.resume();
        try {
          if (bytes.subarray(newline + 1).toString().trim()) throw new Error("unexpected ctlup development output");
          resolve(parseDevelopmentInstallation(JSON.parse(bytes.subarray(0, newline).toString("utf8"))));
        } catch (error) { reject(error); }
      };
      child.stdout.on("data", read);
      void closed.then(() => fail(new Error("ctlup exited without a development result")), fail);
    });
    return {
      installation,
      exited: closed.then(() => { if (!closing) throw new Error("development installation lease ended unexpectedly"); }),
      close: () => {
        close_pending ??= (async () => {
          closing = true;
          child.stdin.end();
          const timer = setTimeout(() => child.kill("SIGTERM"), 5_000);
          try { await closed; } finally { clearTimeout(timer); }
        })();
        return close_pending;
      },
    };
  } catch (error) {
    closing = true;
    child.kill("SIGTERM");
    await closed.catch(() => {});
    throw error;
  }
}
