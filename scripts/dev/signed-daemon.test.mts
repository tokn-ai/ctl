import assert from "node:assert/strict";
import { chmod, copyFile, mkdir, mkdtemp, readFile, readlink, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test, type TestContext } from "node:test";
import { SignedDaemon } from "./signed-daemon.mts";

interface Fixture {
  root: string;
  executable: string;
  log: string;
  supervisor: SignedDaemon;
  packages: number;
  fail_signing: boolean;
}

const unixOnly = { skip: process.platform === "win32" };

async function fixture(context: TestContext): Promise<Fixture> {
  const root = await mkdtemp(path.join(tmpdir(), "ctld-supervisor-"));
  const profile = path.join(root, "profile");
  await writeFile(profile, "test provisioning profile");
  const result: Fixture = {
    root,
    executable: path.join(root, "ctld"),
    log: path.join(root, "processes.jsonl"),
    packages: 0,
    fail_signing: false,
    supervisor: undefined as unknown as SignedDaemon,
  };
  result.supervisor = new SignedDaemon({
    runtime_directory: path.join(root, "runtime"),
    profile_path: profile,
    app_version: "0.1.0",
    repository_root: root,
  }, {
    startup_timeout_ms: 2_000,
    shutdown_timeout_ms: 300,
    package_bundle: async (executable, bundle) => {
      result.packages += 1;
      if (result.fail_signing) {
        throw new Error("test signing failed");
      }
      const output = path.join(bundle, "Contents/MacOS/ctld");
      await mkdir(path.dirname(output), { recursive: true });
      await copyFile(executable, output);
    },
  });
  context.after(async () => {
    await result.supervisor.close();
    await rm(root, { recursive: true, force: true });
  });
  await writeHelper(result, 5);
  return result;
}

async function writeHelper(fixture: Fixture, protocolVersion: number, mode = "ready"): Promise<void> {
  await writeFile(fixture.executable, `#!${process.execPath}
const fs = require("node:fs");
const net = require("node:net");
const protocol_version = ${protocolVersion};
const mode = ${JSON.stringify(mode)};
if (process.argv.includes("--protocol-version")) {
  console.log(protocol_version);
  process.exit(0);
}
const socket = process.argv[process.argv.indexOf("--socket") + 1];
fs.appendFileSync(${JSON.stringify(fixture.log)}, JSON.stringify({ pid: process.pid, protocol_version }) + "\\n");
if (mode === "exit") { process.exit(1); }
process.on("SIGTERM", () => {
  if (mode !== "ignore_term") { process.exit(0); }
});
net.createServer((connection) => {
  connection.on("error", () => {});
  if (mode === "hang") { return; }
  let received = Buffer.alloc(0);
  connection.on("data", (chunk) => {
    received = Buffer.concat([received, chunk]);
    if (received.length < 4 || received.length < received.readUInt32BE() + 4) { return; }
    const request = JSON.parse(received.subarray(4).toString());
    const accepted = mode !== "mismatch" && request.protocol_version === protocol_version;
    const payload = Buffer.from(JSON.stringify(accepted
      ? { type: "handshake_accepted", protocol_version }
      : { type: "error", message: "test protocol mismatch" }));
    const header = Buffer.alloc(4);
    header.writeUInt32BE(payload.length);
    connection.write(header.subarray(0, 2));
    setTimeout(() => connection.end(Buffer.concat([header.subarray(2), payload])), 5);
  });
}).listen(socket);
`);
  await chmod(fixture.executable, 0o700);
}

async function starts(fixture: Fixture): Promise<Array<{ pid: number; protocol_version: number }>> {
  return (await readFile(fixture.log, "utf8")).trim().split("\n").map((line) => JSON.parse(line));
}

function running(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ESRCH") { return false; }
    throw error;
  }
}

test("reuses an unchanged signed daemon across serialized native reloads", unixOnly, async (context) => {
  const fixtureData = await fixture(context);
  await Promise.all([
    fixtureData.supervisor.prepare(fixtureData.executable),
    fixtureData.supervisor.prepare(fixtureData.executable),
    fixtureData.supervisor.prepare(fixtureData.executable),
  ]);
  assert.equal(fixtureData.packages, 1);
  const processes = await starts(fixtureData);
  assert.equal(processes.length, 1);
  assert.equal(running(processes[0].pid), true);
  assert.equal((await stat(fixtureData.supervisor.socket_path)).isSocket(), true);
});

test("replaces protocol 5 with protocol 6 before returning from prepare", unixOnly, async (context) => {
  const fixtureData = await fixture(context);
  await fixtureData.supervisor.prepare(fixtureData.executable);
  const originalLink = await readlink(path.join(fixtureData.root, "runtime/ctld.app"));
  const [original] = await starts(fixtureData);
  await writeHelper(fixtureData, 6);
  await fixtureData.supervisor.prepare(fixtureData.executable);
  const processes = await starts(fixtureData);
  assert.deepEqual(processes.map((item) => item.protocol_version), [5, 6]);
  assert.equal(running(original.pid), false);
  assert.equal(running(processes[1].pid), true);
  assert.equal(fixtureData.packages, 2);
  assert.notEqual(await readlink(path.join(fixtureData.root, "runtime/ctld.app")), originalLink);
});

test("a signing failure preserves the live daemon and its executable", unixOnly, async (context) => {
  const fixtureData = await fixture(context);
  await fixtureData.supervisor.prepare(fixtureData.executable);
  const originalLink = await readlink(path.join(fixtureData.root, "runtime/ctld.app"));
  const [original] = await starts(fixtureData);
  await writeHelper(fixtureData, 6);
  fixtureData.fail_signing = true;
  await assert.rejects(fixtureData.supervisor.prepare(fixtureData.executable), /test signing failed/);
  assert.equal(running(original.pid), true);
  assert.equal(await readlink(path.join(fixtureData.root, "runtime/ctld.app")), originalLink);
  fixtureData.fail_signing = false;
  await fixtureData.supervisor.prepare(fixtureData.executable);
  assert.equal(running(original.pid), false);
  assert.deepEqual((await starts(fixtureData)).map((item) => item.protocol_version), [5, 6]);
});

for (const mode of ["mismatch", "hang", "exit"]) {
  test(`cleans up a daemon that fails readiness (${mode})`, unixOnly, async (context) => {
    const fixtureData = await fixture(context);
    await writeHelper(fixtureData, 6, mode);
    await assert.rejects(
      fixtureData.supervisor.prepare(fixtureData.executable),
      /protocol mismatch|handshake timed out|stopped during startup/,
    );
    const [process] = await starts(fixtureData);
    assert.equal(running(process.pid), false);
    await assert.rejects(stat(fixtureData.supervisor.socket_path), { code: "ENOENT" });
    await writeHelper(fixtureData, 6);
    await fixtureData.supervisor.prepare(fixtureData.executable);
    assert.equal((await starts(fixtureData)).length, 2);
  });
}

test("close waits for preparation and terminates only its owned daemon", unixOnly, async (context) => {
  const fixtureData = await fixture(context);
  await writeHelper(fixtureData, 6, "ignore_term");
  const preparation = fixtureData.supervisor.prepare(fixtureData.executable);
  const closing = fixtureData.supervisor.close();
  await preparation;
  await closing;
  const [process] = await starts(fixtureData);
  assert.equal(running(process.pid), false);
  await assert.rejects(stat(fixtureData.supervisor.socket_path), { code: "ENOENT" });
  await assert.rejects(stat(fixtureData.supervisor.executable), { code: "ENOENT" });
  await assert.rejects(fixtureData.supervisor.prepare(fixtureData.executable), /supervisor is closing/);
  await fixtureData.supervisor.close();
});

test("restarts an unchanged binary if its previous process has exited", unixOnly, async (context) => {
  const fixtureData = await fixture(context);
  await fixtureData.supervisor.prepare(fixtureData.executable);
  const [original] = await starts(fixtureData);
  process.kill(original.pid, "SIGTERM");
  for (let attempt = 0; attempt < 100 && running(original.pid); attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.equal(running(original.pid), false);
  await fixtureData.supervisor.prepare(fixtureData.executable);
  assert.equal((await starts(fixtureData)).length, 2);
});
