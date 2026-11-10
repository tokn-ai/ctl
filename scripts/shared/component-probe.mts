import { execFile, type ExecFileException } from "node:child_process";
import { basename } from "node:path";
import { isDeepStrictEqual, promisify } from "node:util";
import { readAgentFile } from "./agent-bundle.mts";
import { isLinuxBundleTarget, readElfComponent } from "./elf-component.mts";
import { maxComponentMetadata, parseComponent } from "./protocol-contract.mts";

const execute = promisify(execFile);
const defaultTimeoutMs = 5_000;
const maxTimerMs = 2_147_483_647;
const maxDiagnosticBytes = 2_048;

interface ProbeOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  target?: string;
  timeout_ms?: number;
}

function probeTimeout(options: ProbeOptions): number {
  const configured = options.timeout_ms ?? (options.env ?? process.env).CTL_COMPONENT_PROBE_TIMEOUT_MS ?? defaultTimeoutMs;
  const timeout = Number(configured);
  if ((typeof configured === "string" && !/^[0-9]+$/.test(configured)) ||
    !Number.isInteger(timeout) || timeout < 1 || timeout > maxTimerMs) {
    throw new Error(`Invalid component probe timeout. CTL_COMPONENT_PROBE_TIMEOUT_MS must be an integer between 1 and ${maxTimerMs}.`);
  }
  return timeout;
}

function diagnosticStderr(stderr: string): string {
  const bytes = Buffer.from(stderr);
  const excerpt = bytes.subarray(0, maxDiagnosticBytes).toString("utf8");
  return JSON.stringify(excerpt) + (bytes.length > maxDiagnosticBytes ? " (truncated)" : "");
}

/** Inspect the staged executable itself; callers validate the returned component metadata. */
export async function probeComponent(command: string, args: string[], options: ProbeOptions = {}): Promise<{ stdout: string; stderr: string }> {
  const timeout_ms = probeTimeout(options);
  const started = performance.now();
  const context = (elapsed_ms: number) => [
    `executable=${basename(command)}`,
    ...(options.target ? [`target=${options.target}`] : []),
    `host=${process.platform}/${process.arch}`, `elapsed_ms=${elapsed_ms}`, `timeout_ms=${timeout_ms}`,
  ].join(" ");
  const execution = execute(command, args, {
    cwd: options.cwd, env: options.env, encoding: "utf8", maxBuffer: maxComponentMetadata, killSignal: "SIGKILL",
  });
  const child = execution.child;
  let timed_out = false;
  const timer = setTimeout(() => {
    timed_out = true;
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    // A descendant can retain the write ends after the component exits.
    // Close our reads so execFile finishes collecting output at the deadline.
    child.stdout?.destroy();
    child.stderr?.destroy();
  }, timeout_ms);
  try {
    const result = await execution;
    const elapsed = performance.now() - started;
    if (timed_out || elapsed >= timeout_ms) {
      timed_out = true;
      throw Object.assign(new Error("Component output did not close before the probe deadline."), { stderr: result.stderr });
    }
    const elapsed_ms = Math.round(elapsed);
    if (elapsed_ms > defaultTimeoutMs) console.warn(`Slow component metadata probe: ${context(elapsed_ms)}`);
    return result;
  } catch (cause) {
    const error = cause as ExecFileException & { stderr?: string };
    const signal = error.signal ?? child.signalCode;
    let reason: string;
    if (error.code === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER") reason = `exceeded the ${maxComponentMetadata}-byte output limit`;
    else if (timed_out) reason = "timed out";
    else if (signal) reason = "terminated by signal";
    else if (typeof error.code === "string") reason = "could not launch";
    else reason = "exited unsuccessfully";
    const fields = [
      context(Math.round(performance.now() - started)),
      `code=${error.code ?? child.exitCode}`,
      `signal=${signal}`,
      `killed=${error.killed === true || child.killed}`,
      `stderr=${diagnosticStderr(error.stderr ?? "")}`,
    ];
    throw new Error(`Component metadata probe ${reason}: ${fields.join(" ")}`, { cause });
  } finally {
    clearTimeout(timer);
  }
}

/** Validate that the linked record used by cross-builds matches the live binary. */
export async function probeBundledComponent(command: string, args: string[], options: ProbeOptions): Promise<{ stdout: string; stderr: string }> {
  const result = await probeComponent(command, args, options);
  if (options.target && isLinuxBundleTarget(options.target)) {
    const name = basename(command);
    const binary = await readAgentFile(command, 128 * 1024 * 1024);
    const embedded = readElfComponent(binary, options.target, name);
    if (!isDeepStrictEqual(embedded, parseComponent(JSON.parse(result.stdout)))) {
      throw new Error(`${name}: linked component metadata disagrees with --component-info`);
    }
  }
  return result;
}
