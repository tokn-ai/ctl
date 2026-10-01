import { spawn, type ChildProcess } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { constants } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildDaemons } from "./daemon-build.mts";
import { resolveRunnerTarget } from "./signed-cargo-target.mts";

const script = fileURLToPath(import.meta.url);
const nodeArguments = ["--experimental-strip-types", "--disable-warning=ExperimentalWarning"];

export function withAppRunner(cargo_args: string[], target: string): string[] {
  const runner = [process.execPath, ...nodeArguments, fileURLToPath(new URL("./signed-app-relay.mts", import.meta.url))];
  const separator = cargo_args.indexOf("--");
  const insertion = separator === -1 ? cargo_args.length : separator;
  return [...cargo_args.slice(0, insertion), "--config", `target.${JSON.stringify(target)}.runner=${JSON.stringify(runner)}`, ...cargo_args.slice(insertion)];
}

async function status(child: ChildProcess): Promise<number> {
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => resolve(code ?? (signal ? 128 + constants.signals[signal] : 1)));
  });
}

async function worker(cargo_args: string[]): Promise<number> {
  // Tauri kills its runner with SIGKILL. Only this worker owns the build group;
  // a pipe held by the runner makes abrupt parent death observable here.
  const canceled = () => process.kill(-process.pid, "SIGKILL");
  process.stdin.once("end", canceled);
  process.stdin.once("error", canceled);
  process.stdin.resume();
  let directory: string | undefined;
  try {
    const supervisor = process.env.CTMUX_DEV_APP_SUPERVISOR;
    if (!supervisor) throw new Error("Signed app supervisor is unavailable");
    directory = await mkdtemp(path.join(path.dirname(supervisor), "cargo-"));
    const marker = path.join(directory, "launch-state");
    const artifacts = await buildDaemons(cargo_args, process.cwd(), process.env);
    const target = await resolveRunnerTarget(cargo_args, artifacts, process.cwd(), process.env);
    const cargo = spawn("cargo", withAppRunner(cargo_args, target), {
      cwd: process.cwd(),
      env: { ...process.env, CTMUX_DEV_CTLD_EXECUTABLE: artifacts.ctld, CTMUX_DEV_APP_FAILURE_MARKER: marker },
      stdio: ["ignore", "inherit", "inherit"],
    });
    const code = await status(cargo);
    const phase = await readFile(marker, "utf8").catch(() => undefined);
    if (phase === "started") return code;
    throw new Error(code === 0 ? "Cargo did not launch the signed app relay" : `Cargo or signed app preparation exited with status ${code}`);
  } catch (error) {
    console.error(error instanceof Error ? error.message : "Signed app preparation failed");
    // This exact final line/exit status is Tauri's recoverable failure contract.
    console.error("error: could not compile signed development app; keeping the current app running");
    return 101;
  } finally {
    if (directory) await rm(directory, { recursive: true, force: true });
    process.stdin.off("end", canceled);
    process.stdin.off("error", canceled);
    process.stdin.destroy();
  }
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args[0] === "--worker") {
    process.exitCode = await worker(args.slice(1));
    return;
  }
  const child = spawn(process.execPath, [...nodeArguments, script, "--worker", ...args], {
    cwd: process.cwd(), env: process.env, detached: true, stdio: ["pipe", "inherit", "inherit"],
  });
  const cancel = () => child.stdin?.destroy();
  process.once("SIGINT", cancel);
  process.once("SIGTERM", cancel);
  try {
    process.exitCode = await status(child);
  } finally {
    cancel();
    process.off("SIGINT", cancel);
    process.off("SIGTERM", cancel);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === script) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : "Signed Cargo runner failed");
    console.error("error: could not compile signed development app; keeping the current app running");
    process.exitCode = 101;
  });
}
