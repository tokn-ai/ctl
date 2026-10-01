import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, readdir, realpath, rm, stat, writeFile } from "node:fs/promises";
import { createConnection, type Socket } from "node:net";
import path from "node:path";
import { test, type TestContext } from "node:test";
import { encodeAppMessage, maxAppMessageBytes, parseAppLaunchRequest, parseAppSupervisorResponse, type AppLaunchRequest, type AppSupervisorResponse } from "./signed-app-protocol.mts";
import { serveAppSupervisor } from "./signed-app-supervisor.mts";

const unixOnly = { skip: process.platform === "win32", timeout: 10_000 };
const pause = (ms = 10) => new Promise((resolve) => setTimeout(resolve, ms));

interface Fixture {
  root: string;
  socket_path: string;
  request: AppLaunchRequest;
  exits: Array<{ code: number | null; signal: NodeJS.Signals | null }>;
  errors: unknown[];
  prepare: (executable: string) => Promise<void>;
  supervisor: Awaited<ReturnType<typeof serveAppSupervisor>>;
}

interface Record {
  type: "start" | "stop";
  pid: number;
  snapshot: string;
  args: string[];
  cwd: string;
  value: string;
}

async function fixture(context: TestContext): Promise<Fixture> {
  const root = await realpath(await mkdtemp("/tmp/ctmux-app-test-"));
  const executable = path.join(root, "app.cjs");
  const log = path.join(root, "events.jsonl");
  await writeFile(executable, `#!${process.execPath}
const fs = require("node:fs");
const report = (type) => fs.appendFileSync(process.env.FIXTURE_LOG, JSON.stringify({type, pid:process.pid, snapshot:process.argv[1],args:process.argv.slice(2),cwd:process.cwd(),value:process.env.FIXTURE_VALUE}) + "\\n");
process.on("SIGTERM", () => { report("stop"); setTimeout(() => process.exit(0), Number(process.env.FIXTURE_STOP_DELAY || "0")); });
process.on("SIGUSR1", () => process.exit(7));
setInterval(() => {}, 1000);
// The parent may signal us as soon as it observes this readiness record.
// Install handlers first; Node's default SIGUSR1 action starts its debugger.
report("start");
`);
  await chmod(executable, 0o700);
  const result: Fixture = {
    root, socket_path: path.join(root, "app.sock"), exits: [], errors: [],
    request: { executable, args: ["two words", "--fixture"], cwd: root, ctld_executable: path.join(root, "ctld"), env: { FIXTURE_LOG: log, FIXTURE_VALUE: "inherited fixture value" } },
    prepare: async () => {}, supervisor: undefined as unknown as Fixture["supervisor"],
  };
  result.supervisor = await serveAppSupervisor(result.socket_path, {
    prepare: (executable) => result.prepare(executable),
    on_exit: (status) => { result.exits.push(status); },
    on_error: (error) => result.errors.push(error),
    shutdown_timeout_ms: 200,
  });
  context.after(async () => {
    await result.supervisor.close();
    await rm(root, { recursive: true, force: true });
  });
  return result;
}

function client(data: Fixture, request = data.request): { socket: Socket; next: () => Promise<AppSupervisorResponse> } {
  const socket = createConnection(data.socket_path);
  const messages: AppSupervisorResponse[] = [];
  const waiters: Array<(message: AppSupervisorResponse) => void> = [];
  let received = "";
  socket.setEncoding("utf8");
  socket.on("error", () => {});
  socket.on("data", (chunk: string) => {
    received += chunk;
    for (let end = received.indexOf("\n"); end >= 0; end = received.indexOf("\n")) {
      const message = parseAppSupervisorResponse(JSON.parse(received.slice(0, end)));
      received = received.slice(end + 1);
      const waiter = waiters.shift();
      if (waiter) waiter(message); else messages.push(message);
    }
  });
  socket.write(encodeAppMessage(request));
  return { socket, next: () => messages.length ? Promise.resolve(messages.shift()!) : new Promise((resolve) => waiters.push(resolve)) };
}

async function records(data: Fixture): Promise<Record[]> {
  try { return (await readFile(path.join(data.root, "events.jsonl"), "utf8")).trim().split("\n").map((line) => JSON.parse(line)); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
}

async function waitRecords(data: Fixture, type: Record["type"], count: number): Promise<Record[]> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    const found = (await records(data)).filter((record) => record.type === type);
    if (found.length >= count) return found;
    await pause();
  }
  assert.fail(`Fixture did not report ${count} ${type} events`);
}

function running(pid: number): boolean {
  try { process.kill(pid, 0); return true; }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ESRCH") return false; throw error; }
}

function gate(): { entered: Promise<void>; enter: () => Promise<void>; release: () => void } {
  let entered!: () => void;
  let release!: () => void;
  const entry = new Promise<void>((resolve) => { entered = resolve; });
  const waiting = new Promise<void>((resolve) => { release = resolve; });
  return { entered: entry, enter: async () => { entered(); await waiting; }, release };
}

test("launches an adjacent independent snapshot with exact runtime context", unixOnly, async (context) => {
  const data = await fixture(context);
  const relay = client(data);
  assert.deepEqual(await relay.next(), { type: "started" });
  const [app] = await waitRecords(data, "start", 1);
  assert.notEqual(app.snapshot, data.request.executable);
  assert.equal(path.dirname(app.snapshot), path.dirname(data.request.executable));
  assert.deepEqual(app.args, data.request.args);
  assert.equal(app.cwd, data.request.cwd);
  assert.equal(app.value, data.request.env.FIXTURE_VALUE);
  assert.equal((await stat(data.socket_path)).mode & 0o077, 0);
  await writeFile(data.request.executable, "new broken compilation output");
  assert.match(await readFile(app.snapshot, "utf8"), /report\("start"\)/);
  relay.socket.destroy();
  await pause(30);
  assert.equal(running(app.pid), true);
  await data.supervisor.close();
  assert.equal(running(app.pid), false);
  assert.deepEqual(data.exits, []);
  assert.deepEqual((await readdir(data.root)).filter((name) => name.startsWith(".ctmux-app-dev-")), []);
});

test("preparation failure preserves the current app and surfaces only safe client error", unixOnly, async (context) => {
  const data = await fixture(context);
  const original = client(data);
  await original.next();
  const [app] = await waitRecords(data, "start", 1);
  data.prepare = async () => { throw new Error("fixture signing profile detail"); };
  const next = client(data);
  assert.deepEqual(await next.next(), { type: "error", message: "Signed daemon preparation failed; the current app was preserved" });
  assert.equal(running(app.pid), true);
  assert.equal(data.errors.length, 1);
  assert.deepEqual(data.exits, []);
  assert.equal((await records(data)).filter((record) => record.type === "start").length, 1);
});

test("disconnect during preparation cancels replacement and cleans its snapshot", unixOnly, async (context) => {
  const data = await fixture(context);
  await client(data).next();
  const [original] = await waitRecords(data, "start", 1);
  const preparation = gate();
  data.prepare = preparation.enter;
  const relay = client(data);
  await preparation.entered;
  relay.socket.destroy();
  await pause(30);
  preparation.release();
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if ((await readdir(data.root)).filter((name) => name.startsWith(".ctmux-app-dev-")).length === 1) break;
    await pause();
  }
  assert.equal(running(original.pid), true);
  assert.equal((await readdir(data.root)).filter((name) => name.startsWith(".ctmux-app-dev-")).length, 1);
  assert.equal((await records(data)).filter((record) => record.type === "start").length, 1);
});

test("successful preparation replaces only its owned app without reporting natural exit", unixOnly, async (context) => {
  const data = await fixture(context);
  const old = client(data);
  await old.next();
  const [original] = await waitRecords(data, "start", 1);
  old.socket.destroy();
  const next = client(data);
  assert.deepEqual(await next.next(), { type: "started" });
  const [, replacement] = await waitRecords(data, "start", 2);
  assert.equal(running(original.pid), false);
  assert.equal(running(replacement.pid), true);
  assert.deepEqual(data.exits, []);
  await assert.rejects(stat(original.snapshot), { code: "ENOENT" });
});

test("only the newest overlapping successful build replaces the app", unixOnly, async (context) => {
  const data = await fixture(context);
  await client(data).next();
  const [original] = await waitRecords(data, "start", 1);
  const preparation = gate();
  let calls = 0;
  data.prepare = async () => { if (++calls === 1) await preparation.enter(); };
  const superseded = client(data, { ...data.request, args: ["superseded"] });
  await preparation.entered;
  const newest = client(data, { ...data.request, args: ["newest"] });
  await new Promise<void>((resolve) => newest.socket.once("connect", resolve));
  // Let the socket's written request pass through the server's I/O turn before
  // releasing preparation; no build or process timing is assumed here.
  await new Promise<void>((resolve) => setImmediate(resolve));
  await new Promise<void>((resolve) => setImmediate(resolve));
  preparation.release();
  assert.deepEqual(await superseded.next(), { type: "error", message: "This app launch was superseded by a newer build" });
  assert.deepEqual(await newest.next(), { type: "started" });
  const started = await waitRecords(data, "start", 2);
  assert.equal(started.length, 2);
  assert.deepEqual(started[1].args, ["newest"]);
  assert.equal(running(original.pid), false);
  assert.deepEqual(data.exits, []);
});

test("relay disconnect after replacement commits still completes the launch", unixOnly, async (context) => {
  const data = await fixture(context);
  data.request.env.FIXTURE_STOP_DELAY = "100";
  await client(data).next();
  const [original] = await waitRecords(data, "start", 1);
  const next = client(data);
  await waitRecords(data, "stop", 1);
  next.socket.destroy();
  const [, replacement] = await waitRecords(data, "start", 2);
  assert.equal(running(original.pid), false);
  assert.equal(running(replacement.pid), true);
  assert.deepEqual(data.exits, []);
});

for (const disconnect of [false, true]) {
  test(`natural app exit reports status even when relay disconnected=${disconnect}`, unixOnly, async (context) => {
    const data = await fixture(context);
    const relay = client(data);
    await relay.next();
    const [app] = await waitRecords(data, "start", 1);
    if (disconnect) relay.socket.destroy();
    process.kill(app.pid, "SIGUSR1");
    if (!disconnect) assert.deepEqual(await relay.next(), { type: "exit", code: 7, signal: null });
    for (let attempt = 0; attempt < 100 && !data.exits.length; attempt += 1) await pause();
    assert.deepEqual(data.exits, [{ code: 7, signal: null }]);
    await assert.rejects(stat(app.snapshot), { code: "ENOENT" });
  });
}

test("shutdown during preparation cancels new launch and drains cleanup", unixOnly, async (context) => {
  const data = await fixture(context);
  await client(data).next();
  const [app] = await waitRecords(data, "start", 1);
  const preparation = gate();
  data.prepare = preparation.enter;
  client(data);
  await preparation.entered;
  const closed = data.supervisor.close();
  preparation.release();
  await closed;
  assert.equal(running(app.pid), false);
  assert.deepEqual((await readdir(data.root)).filter((name) => name.startsWith(".ctmux-app-dev-")), []);
  assert.deepEqual(data.exits, []);
});

test("failed operating-system launch reports error and cleans snapshot", unixOnly, async (context) => {
  const data = await fixture(context);
  await writeFile(data.request.executable, "#!/missing/fixture/interpreter\n");
  const relay = client(data);
  assert.deepEqual(await relay.next(), { type: "error", message: "The successfully built app could not be started" });
  await data.supervisor.close();
  assert.deepEqual((await readdir(data.root)).filter((name) => name.startsWith(".ctmux-app-dev-")), []);
  assert.deepEqual(data.exits, []);
});

test("launch protocol rejects malformed paths, environment and responses", () => {
  const request: AppLaunchRequest = { executable: "/fixture/app", args: [], cwd: "/fixture", env: { VALUE: "value" }, ctld_executable: "/fixture/ctld" };
  assert.deepEqual(parseAppLaunchRequest(JSON.parse(encodeAppMessage(request))), request);
  for (const changed of [{ executable: "relative" }, { args: ["bad\0argument"] }, { env: { "BAD=NAME": "value" } }, { env: { BAD: 7 } }]) {
    assert.throws(() => parseAppLaunchRequest({ ...request, ...changed }), /Invalid signed app launch request/);
  }
  assert.throws(() => encodeAppMessage({ ...request, args: ["a".repeat(maxAppMessageBytes)] }), /size limit/);
  assert.throws(() => parseAppSupervisorResponse({ type: "exit", code: -1, signal: null }), /Invalid/);
  assert.throws(() => parseAppSupervisorResponse({ type: "exit", code: null, signal: "invented" }), /Invalid/);
});

test("bounded server rejects oversized unframed request without launching", unixOnly, async (context) => {
  const data = await fixture(context);
  const socket = createConnection(data.socket_path);
  socket.on("error", () => {});
  const response = new Promise<string>((resolve) => socket.once("data", (chunk) => resolve(chunk.toString())));
  socket.write("a".repeat(maxAppMessageBytes + 1));
  assert.equal(parseAppSupervisorResponse(JSON.parse(await response)).type, "error");
  socket.destroy();
  assert.deepEqual(await records(data), []);
});
