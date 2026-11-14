import { spawn } from "node:child_process";
import { access, cp, mkdir, mkdtemp, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repository_root = fileURLToPath(new URL("../../", import.meta.url));
const demos = ["persistence", "remote", "vpn"] as const;
const help = `Landing-page demo tools (run with pnpm demo):
  prepare
  record <persistence|remote|vpn> --take <name> --rect <x,y,width,height> [--seconds 45]
  export <persistence|remote|vpn> --take <name> --start <seconds> --duration <seconds>

prepare creates a fresh sample workspace under /tmp; it never starts a session.
record captures the selected rectangle on macOS after a five-second countdown.
export requires ffmpeg and writes a silent 1280x800 MP4 and a PNG poster.
Recordings stay in ignored .demo-recordings/. See docs/demos.md for the shot lists.
`;

export function parseOptions(args: string[]): Record<string, string> {
  const options: Record<string, string> = {};
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    const value = args[index + 1];
    if (!key?.startsWith("--") || value === undefined || value.startsWith("--") || key in options) {
      throw new Error("Expected unique --option value pairs.");
    }
    options[key] = value;
  }
  return options;
}

export function rectangle(value: string): string {
  if (!/^-?\d+,-?\d+,\d+,\d+$/.test(value)) throw new Error("Use --rect x,y,width,height in screen points.");
  const values = value.split(",").map(Number);
  if (!values.every(Number.isSafeInteger) || values[2]! < 2 || values[3]! < 2) {
    throw new Error("Capture width and height must be at least two points.");
  }
  return values.join(",");
}

function seconds(value: string | undefined, label: string, minimum: number, maximum: number): number {
  const result = Number(value);
  if (value === undefined || value.trim() === "" || !Number.isFinite(result) || result < minimum || result > maximum) {
    throw new Error(`${label} must be between ${minimum} and ${maximum} seconds.`);
  }
  return result;
}

export function takeDirectory(demo: string | undefined, take: string | undefined): string {
  if (!demos.includes(demo as typeof demos[number])) throw new Error("Choose persistence, remote, or vpn.");
  if (!take || !/^[a-z0-9][a-z0-9_-]{0,47}$/.test(take)) throw new Error("Take names must use 1–48 lowercase letters, digits, hyphens, or underscores.");
  return path.join(repository_root, ".demo-recordings", demo!, take);
}

async function run(command: string, args: string[]): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const child = spawn(command, args, { stdio: "inherit" });
    const interrupt = () => child.kill("SIGINT");
    const terminate = () => child.kill("SIGTERM");
    process.on("SIGINT", interrupt);
    process.on("SIGTERM", terminate);
    const cleanup = () => {
      process.off("SIGINT", interrupt);
      process.off("SIGTERM", terminate);
    };
    child.once("error", (error) => { cleanup(); reject(error); });
    child.once("exit", (code, signal) => {
      cleanup();
      if (code === 0) resolve();
      else reject(new Error(`${command} failed (${signal ?? code}).`));
    });
  });
}

async function requireAbsent(file: string): Promise<void> {
  try {
    await access(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }
  throw new Error(`Output already exists: ${path.basename(file)}. Use a new take or move the old output first.`);
}

async function main(args: string[]): Promise<void> {
  const [command, demo, ...rest] = args;
  if (!command || command === "--help" || command === "help") { console.log(help); return; }
  if (command === "prepare") {
    if (args.length !== 1) throw new Error("prepare takes no arguments.");
    if (process.platform === "win32") throw new Error("The demo fixture requires a Unix shell.");
    // /tmp avoids the account-specific path in the macOS temporary directory.
    const workspace = await mkdtemp("/tmp/ctl-demo-");
    await cp(path.join(repository_root, "demos/workspace"), workspace, { recursive: true });
    console.log(`Sample workspace: ${workspace}\n\nRun off camera:\nctmux new -ds demo-local -c '${workspace}' -- /bin/sh '${workspace}/launch.sh'\n\nThen use Attach session in the desktop. See docs/demos.md for remote setup.`);
    return;
  }
  if (command !== "record" && command !== "export") throw new Error(`Unknown command: ${command}`);
  const options = parseOptions(rest);
  const allowed = command === "record" ? ["--take", "--rect", "--seconds"] : ["--take", "--start", "--duration"];
  for (const key of Object.keys(options)) if (!allowed.includes(key)) throw new Error(`Unknown option: ${key}`);
  const directory = takeDirectory(demo, options["--take"]);
  const source = path.join(directory, "source.mov");
  if (command === "record") {
    if (process.platform !== "darwin") throw new Error("Automated capture uses macOS screencapture. See docs/demos.md for other platforms.");
    const capture_rect = rectangle(options["--rect"] ?? "");
    const capture_seconds = seconds(options["--seconds"] ?? "45", "Capture length", 1, 180);
    if (!Number.isInteger(capture_seconds)) throw new Error("Capture length must be a whole number of seconds.");
    await mkdir(path.dirname(directory), { recursive: true });
    await mkdir(directory); // Never overwrite an earlier take, including an interrupted one.
    console.log("Recording starts in five seconds. Focus the demo window; audio is disabled.");
    await new Promise((resolve) => setTimeout(resolve, 5000));
    await run("/usr/sbin/screencapture", ["-v", "-k", `-V${capture_seconds}`, `-R${capture_rect}`, source]);
    if ((await stat(source)).size === 0) throw new Error("Capture produced an empty recording.");
    await writeFile(path.join(directory, "capture.json"), `${JSON.stringify({
      demo, take: options["--take"], capture_rect, capture_seconds,
    }, null, 2)}\n`, { flag: "wx" });
    console.log("Capture saved. Review source.mov before choosing export trim points.");
    return;
  }
  const start = seconds(options["--start"], "Trim start", 0, 180);
  const duration = seconds(options["--duration"], "Trim duration", 1, 90);
  const metadata = JSON.parse(await readFile(path.join(directory, "capture.json"), "utf8")) as { capture_seconds: number };
  if (!Number.isFinite(metadata.capture_seconds) || start + duration > metadata.capture_seconds) {
    throw new Error("Trim extends past the configured capture length.");
  }
  const video = path.join(directory, "demo.mp4");
  const poster = path.join(directory, "poster.png");
  const edit = path.join(directory, "edit.json");
  for (const file of [video, poster, edit]) await requireAbsent(file);
  await run("ffmpeg", ["-hide_banner", "-loglevel", "error", "-n", "-ss", String(start), "-i", source,
    "-t", String(duration), "-map", "0:v:0", "-an", "-map_metadata", "-1",
    "-vf", "scale=1280:800:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1280:800:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30",
    "-c:v", "libx264", "-crf", "20", "-preset", "medium", "-pix_fmt", "yuv420p", "-movflags", "+faststart", video]);
  await run("ffmpeg", ["-hide_banner", "-loglevel", "error", "-n", "-i", video, "-frames:v", "1", "-update", "1", poster]);
  await writeFile(edit, `${JSON.stringify({ start_seconds: start, duration_seconds: duration }, null, 2)}\n`, { flag: "wx" });
  console.log("Exported demo.mp4 and poster.png. Review both before using them on the landing page.");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
