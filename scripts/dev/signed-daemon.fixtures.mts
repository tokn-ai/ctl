import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { appendFile, chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import type { TestContext } from "node:test";
import type { ProtocolInfo } from "../shared/protocol-contract.mts";
import { inspectDaemon } from "./signed-daemon-probe.mts";
import { SignedDaemon, type SignedDaemonConfig } from "./signed-daemon.mts";

export interface Fixture {
  root: string;
  executable: string;
  log: string;
  package_log: string;
  config: SignedDaemonConfig;
  supervisor: SignedDaemon;
  diagnostics: string[];
  fail_signing: boolean;
}

export const unixOnly = { skip: process.platform === "win32", timeout: 15_000 };

export async function fixture(context: TestContext): Promise<Fixture> {
  const root = await mkdtemp("/tmp/ctld-supervisor-");
  const profile = path.join(root, "profile");
  await writeFile(profile, "test provisioning profile");
  for (const relative of ["scripts/ci/package-ctld-app.sh", "apps/desktop/src-tauri/macos/ctld/Info.plist", "apps/desktop/src-tauri/macos/ctld/Entitlements.plist"]) {
    const file = path.join(root, relative);
    await mkdir(path.dirname(file), { recursive: true });
    await writeFile(file, "test signing recipe");
  }
  const result: Fixture = {
    root,
    executable: path.join(root, "ctld"),
    log: path.join(root, "processes.jsonl"),
    package_log: path.join(root, "packages.jsonl"),
    config: { runtime_directory: path.join(root, "runtime"), profile_path: profile, app_version: "0.1.0", repository_root: root },
    diagnostics: [],
    fail_signing: false,
    supervisor: undefined as unknown as SignedDaemon,
  };
  result.supervisor = supervisor(result);
  context.after(async () => {
    await result.supervisor.close();
    for (const child of await starts(result)) await stopFixture(child.pid);
    await rm(root, { recursive: true, force: true });
  });
  await writeHelper(result, 5);
  return result;
}

export function supervisor(data: Fixture): SignedDaemon {
  return new SignedDaemon(data.config, {
    startup_timeout_ms: 800,
    shutdown_timeout_ms: 200,
    on_diagnostic: (message) => data.diagnostics.push(message),
    package_bundle: async (executable, bundle) => {
      await appendFile(data.package_log, "package\n");
      if (data.fail_signing) throw new Error("test signing failed");
      const output = path.join(bundle, "Contents/MacOS/ctld");
      await mkdir(path.dirname(output), { recursive: true });
      await copyFile(executable, output);
    },
  });
}

export async function writeHelper(data: Fixture, protocol_build: number, mode = "ready", contract?: ProtocolInfo): Promise<void> {
  await writeFile(data.executable, `#!${process.execPath}
const fs = require("node:fs");
const net = require("node:net");
const protocol = ${JSON.stringify(contract ?? {name:"ctld",build:protocol_build,version:`1.0.${protocol_build}`,supported_versions:[`1.0.${protocol_build}`]})};
const protocol_version = protocol.version;
const protocols = [protocol, ...["ctld_lifecycle", "ctld_helper"].map((name) => ({name, build:1, version:"1.0.1", supported_versions:["1.0.1"]}))];
const build = {version:"0.1.0",source_revision:"a".repeat(40),source_fingerprint:"b".repeat(64),dirty:false};
if (process.argv.includes("--component-info")) {
  console.log(JSON.stringify({build, protocols}));
  process.exit(0);
}
if (process.argv.includes("--protocol-build")) {
  console.log(protocol.build);
  process.exit(0);
}
const mode = ${JSON.stringify(mode)};
if (process.argv.includes("--protocol-version")) {
  console.log(protocol_version);
  process.exit(0);
}
const socket = process.argv[process.argv.indexOf("--socket") + 1];
fs.appendFileSync(${JSON.stringify(data.log)}, JSON.stringify({ pid: process.pid, protocol_version, ctld_bin: process.env.CTLD_BIN }) + "\\n");
if (mode === "exit") process.exit(1);
let owned_inode;
process.on("SIGTERM", () => {
  if (mode === "ignore_term") return;
  try { if (fs.lstatSync(socket).ino === owned_inode) fs.unlinkSync(socket); } catch {}
  process.exit(0);
});
const server = net.createServer((connection) => {
  connection.on("error", () => {});
  if (mode === "hang") return;
  let received = Buffer.alloc(0);
  connection.on("data", (chunk) => {
    received = Buffer.concat([received, chunk]);
    if (received.length < 4 || received.length < received.readUInt32BE() + 4) return;
    const request = JSON.parse(received.subarray(4).toString());
    const accepted = mode !== "mismatch" && request.type === "ctld_inspect" && request.protocol?.supported_versions?.includes("1.0.1");
    const payload = Buffer.from(JSON.stringify(accepted
      ? { type: "ctld_info", protocol_version: "1.0.1", info: { instance_id: "fixture-" + process.pid, binary: { build, protocol_version, lifecycle_protocol_version: "1.0.1", protocols } } }
      : { type: "error", message: "test protocol mismatch" }));
    const header = Buffer.alloc(4);
    header.writeUInt32BE(payload.length);
    connection.write(header.subarray(0, 2));
    setTimeout(() => connection.end(Buffer.concat([header.subarray(2), payload])), 5);
  });
});
server.on("listening", () => {
  fs.chmodSync(socket, 0o600);
  owned_inode = fs.lstatSync(socket).ino;
});
server.on("error", (error) => {
  if (error.code !== "EADDRINUSE") process.exit(1);
  // Model ctld's existing stale-socket recovery, without supervisor unlinking.
  const probe = net.createConnection(socket);
  probe.on("connect", () => process.exit(1));
  probe.on("error", (error) => {
    if (error.code !== "ECONNREFUSED" && error.code !== "ENOENT") process.exit(1);
    try { fs.unlinkSync(socket); } catch (error) { if (error.code !== "ENOENT") process.exit(1); }
    server.listen(socket);
  });
});
if (mode === "replacement_race") {
  require("node:child_process").spawn(${JSON.stringify(data.executable + ".replacement")}, ["--socket", socket], { detached: true, stdio: "ignore" }).unref();
  setInterval(() => {}, 1000);
} else {
  server.listen(socket);
}
`);
  await chmod(data.executable, 0o700);
}

export async function starts(data: Fixture): Promise<Array<{ pid: number; protocol_version: string; ctld_bin?: string }>> {
  try {
    return (await readFile(data.log, "utf8")).trim().split("\n").map((line) => JSON.parse(line));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw error;
  }
}

export async function packages(data: Fixture): Promise<number> {
  return (await readFile(data.package_log, "utf8")).trim().split("\n").length;
}

export function running(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ESRCH") return false;
    throw error;
  }
}

export async function stopFixture(pid: number, signal: "SIGTERM" | "SIGKILL" = "SIGTERM"): Promise<void> {
  for (const next of [signal, "SIGKILL"] as const) {
    if (!running(pid)) return;
    process.kill(pid, next);
    for (let attempt = 0; attempt < 50 && running(pid); attempt += 1) await pause(10);
  }
  assert.equal(running(pid), false, "fixture process must stop");
}

export async function startExternal(data: Fixture, executable = data.executable): Promise<number> {
  const child = spawn(executable, ["--socket", data.supervisor.socket_path], { detached: true, stdio: "ignore" });
  assert.ok(child.pid);
  child.unref();
  return child.pid;
}

export async function waitReady(data: Fixture, protocol_version: number): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if ((await inspectDaemon(data.supervisor.socket_path, 300))?.version === `1.0.${protocol_version}`) return;
    await pause(10);
  }
  assert.fail("fixture daemon did not become ready");
}

export function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
