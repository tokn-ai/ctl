import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { createServer, type Socket } from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test, { type TestContext } from "node:test";
import { serveAppSupervisor } from "./signed-app-supervisor.mts";
import { encodeAppMessage, parseAppLaunchRequest, type AppLaunchRequest } from "./signed-app-protocol.mts";

const runner = fileURLToPath(new URL("./tauri-cargo.sh", import.meta.url));
const subprocess = { skip: process.platform === "win32", timeout: 20_000 };

async function fixture(context: TestContext, target = "fixture-target", profile = "custom") {
  const directory = await realpath(await mkdtemp("/tmp/signed-cargo-"));
  const target_directory = path.join(directory, "target");
  const output = path.join(target_directory, target, profile);
  await mkdir(output, { recursive: true });
  const artifacts = Object.fromEntries(["ctld", "ctmuxd", "ctl-taskd"].map((name) => [name, path.join(output, name)]));
  for (const executable of Object.values(artifacts)) await writeFile(executable, "synthetic helper");
  const executable = path.join(output, "ctmux-app");
  const app_marker = path.join(directory, "app.json");
  await writeFile(executable, `#!${process.execPath}
const fs = require("node:fs");
fs.writeFileSync(process.env.TEST_APP_MARKER + ".tmp", JSON.stringify({pid:process.pid,args:process.argv.slice(2),cwd:process.cwd(),dyld:process.env.DYLD_FALLBACK_LIBRARY_PATH,ctmuxd:process.env.CTMUXD_BIN,ctl_taskd:process.env.CTL_TASKD_BIN}));
fs.renameSync(process.env.TEST_APP_MARKER + ".tmp", process.env.TEST_APP_MARKER);
setInterval(() => {}, 1000);
`, { mode: 0o755 });
  await writeFile(path.join(directory, "cargo"), `#!${process.execPath}
const fs = require("node:fs");
const {spawn} = require("node:child_process");
const args = process.argv.slice(2);
fs.appendFileSync(process.env.TEST_CARGO_CALLS, JSON.stringify(args)+"\\n");
if(args[0] === "build") {
  if(process.env.TEST_HOLD_BUILD === "helper") {
    const grandchild = spawn(process.execPath, ["-e", "setInterval(()=>{},1000)"], {stdio:"ignore"});
    fs.writeFileSync(process.env.TEST_PROCESS_MARKER+".tmp", JSON.stringify({cargo:process.pid,grandchild:grandchild.pid}));
    fs.renameSync(process.env.TEST_PROCESS_MARKER+".tmp",process.env.TEST_PROCESS_MARKER);
    setInterval(()=>{},1000);
  } else if(process.env.TEST_HELPER_FAILURE) {
    console.error("synthetic helper failure"); process.exitCode=101;
  } else for(const [name,executable] of Object.entries(JSON.parse(process.env.TEST_ARTIFACTS))) console.log(JSON.stringify({reason:"compiler-artifact",target:{name,kind:["bin"]},executable}));
} else if(args[0] === "metadata") console.log(JSON.stringify({target_directory:process.env.TEST_TARGET_DIRECTORY}));
else if(args[0] === "run") {
  if(process.env.TEST_HOLD_BUILD === "app") {
    const grandchild = spawn(process.execPath, ["-e", "setInterval(()=>{},1000)"], {stdio:"ignore"});
    fs.writeFileSync(process.env.TEST_PROCESS_MARKER+".tmp", JSON.stringify({cargo:process.pid,grandchild:grandchild.pid}));
    fs.renameSync(process.env.TEST_PROCESS_MARKER+".tmp",process.env.TEST_PROCESS_MARKER);
    setInterval(()=>{},1000);
  } else if(process.env.TEST_APP_FAILURE) { console.error("synthetic app failure"); process.exitCode=101; }
  else {
    const entry=args.findLast((value)=>value.includes(".runner="));
    const runner=JSON.parse(entry.split(".runner=")[1]);
    const separator=args.indexOf("--");
    const relay=spawn(runner[0],[...runner.slice(1),process.env.TEST_APP_EXECUTABLE,...(separator<0?[]:args.slice(separator+1))],{env:{...process.env,DYLD_FALLBACK_LIBRARY_PATH:"/synthetic/cargo/runtime"},stdio:"inherit"});
    relay.once("exit",(code)=>{process.exitCode=code??1; if(code) console.error("Cargo's final non-compiler runner diagnostic");});
  }
}
`, { mode: 0o755 });
  await writeFile(path.join(directory, "rustc"), `#!${process.execPath}\nconsole.log("host: fixture-host");\n`, { mode: 0o755 });
  const socket = path.join(directory, "app.sock");
  const children = new Set<ChildProcess>();
  const env = {
    // Deliberately isolated: no live daemon/credential/runtime environment.
    PATH: [directory, path.dirname(process.execPath), "/usr/bin", "/bin"].join(path.delimiter),
    CTMUX_DEV_APP_SUPERVISOR: socket,
    CTMUXD_BIN: "/synthetic/user-ctmuxd",
    CTL_TASKD_BIN: "/synthetic/user-taskd",
    TEST_TARGET_DIRECTORY: target_directory,
    TEST_ARTIFACTS: JSON.stringify(artifacts),
    TEST_CARGO_CALLS: path.join(directory, "calls.jsonl"),
    TEST_APP_EXECUTABLE: executable,
    TEST_APP_MARKER: app_marker,
    TEST_PROCESS_MARKER: path.join(directory, "processes.json"),
  };
  function launch(args = ["run"], extra: NodeJS.ProcessEnv = {}) {
    const child = spawn(runner, args, { cwd: directory, env: { ...env, ...extra }, stdio: ["ignore", "pipe", "pipe"] });
    children.add(child);
    let diagnostics = "";
    child.stdout.resume();
    child.stderr.on("data", (chunk) => { diagnostics += String(chunk); });
    const closed = new Promise<{ code: number | null; signal: NodeJS.Signals | null; diagnostics: string }>((resolve, reject) => {
      child.once("error", reject);
      child.once("close", (code, signal) => { children.delete(child); resolve({ code, signal, diagnostics }); });
    });
    return { child, closed };
  }
  context.after(async () => {
    for (const child of children) child.kill("SIGKILL");
    await rm(directory, { recursive: true, force: true });
  });
  return { directory, artifacts, socket, env, app_marker, launch };
}

async function until<T>(read: () => Promise<T | undefined>): Promise<T> {
  for (let attempt = 0; attempt < 500; attempt += 1) {
    const result = await read();
    if (result !== undefined) return result;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("synthetic fixture did not become ready");
}

async function readJson<T>(file: string): Promise<T | undefined> {
  try { return JSON.parse(await readFile(file, "utf8")) as T; }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined; throw error; }
}

function running(pid: number): boolean {
  try { process.kill(pid, 0); return true; }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ESRCH") return false; throw error; }
}

async function responder(socket: string, handle: (request: AppLaunchRequest, peer: Socket) => void) {
  const server = createServer((peer) => {
    let contents = "";
    peer.on("data", (chunk) => {
      contents += String(chunk);
      if (contents.includes("\n")) handle(parseAppLaunchRequest(JSON.parse(contents.trim())), peer);
    });
  });
  await new Promise<void>((resolve, reject) => { server.once("error", reject); server.listen(socket, resolve); });
  return { close: () => new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve())) };
}

test("failed first build and signing recover; relay preserves Cargo runtime environment and arguments", subprocess, async (context) => {
  const data = await fixture(context);
  let requests = 0;
  let reject = true;
  const server = await responder(data.socket, (request, peer) => {
    requests += 1;
    assert.equal(request.env.DYLD_FALLBACK_LIBRARY_PATH, "/synthetic/cargo/runtime");
    assert.equal(request.env.CTMUXD_BIN, "/synthetic/user-ctmuxd");
    assert.equal(request.env.CTL_TASKD_BIN, "/synthetic/user-taskd");
    assert.equal(request.ctld_executable, data.artifacts.ctld);
    assert.equal(request.cwd, data.directory);
    assert.deepEqual(request.args, ["with spaces", "--target=app-only"]);
    if (reject) peer.end(encodeAppMessage({ type: "error", message: "synthetic signing failure" }));
    else peer.end(encodeAppMessage({ type: "started" }) + encodeAppMessage({ type: "exit", code: 7, signal: null }));
  });
  context.after(() => server.close());
  const args = ["run", "--target", "fixture-target", "--profile", "custom", "--features", "one,two", "--", "with spaces", "--target=app-only"];
  const first = await data.launch(args, { TEST_HELPER_FAILURE: "1" }).closed;
  assert.equal(first.code, 101, first.diagnostics);
  assert.match(first.diagnostics.trim().split("\n").at(-1)!, /could not compile/);
  assert.equal(requests, 0);
  const signing = await data.launch(args).closed;
  assert.equal(signing.code, 101, signing.diagnostics);
  assert.match(signing.diagnostics.trim().split("\n").at(-1)!, /could not compile/);
  reject = false;
  const recovered = await data.launch(args).closed;
  assert.equal(recovered.code, 7, recovered.diagnostics);
  assert.doesNotMatch(recovered.diagnostics, /could not compile/);
  assert.equal(requests, 2);
});

test("Tauri SIGKILL and failed rebuild preserve the app until a successful replacement", subprocess, async (context) => {
  const data = await fixture(context);
  let preparations = 0;
  const supervisor = await serveAppSupervisor(data.socket, { prepare: async () => { preparations += 1; }, on_exit: () => {} });
  context.after(() => supervisor.close());
  const first = data.launch();
  const old = await until(() => readJson<{ pid: number }>(data.app_marker));
  first.child.kill("SIGKILL");
  await first.closed;
  assert.ok(running(old.pid));
  const failed = await data.launch(["run"], { TEST_APP_FAILURE: "1" }).closed;
  assert.equal(failed.code, 101, failed.diagnostics);
  assert.ok(running(old.pid));
  assert.equal(preparations, 1);
  const second = data.launch();
  const next = await until(async () => { const value = await readJson<{ pid: number }>(data.app_marker); return value?.pid !== old.pid ? value : undefined; });
  assert.ok(!running(old.pid));
  assert.ok(running(next.pid));
  assert.equal(preparations, 2);
  second.child.kill("SIGKILL");
  await second.closed;
  assert.ok(running(next.pid));
  await supervisor.close();
  assert.ok(!running(next.pid));
});

test("Tauri SIGKILL cancels the whole build group including Cargo grandchildren", subprocess, async (context) => {
  for (const phase of ["helper", "app"]) {
    const data = await fixture(context);
    const build = data.launch(["run"], { TEST_HOLD_BUILD: phase });
    const pids = await until(() => readJson<{ cargo: number; grandchild: number }>(data.env.TEST_PROCESS_MARKER));
    assert.ok(running(pids.cargo));
    assert.ok(running(pids.grandchild));
    build.child.kill("SIGKILL");
    await build.closed;
    await until(async () => !running(pids.cargo) && !running(pids.grandchild) ? true : undefined);
  }
});

test("implicit host target uses the Cargo runtime relay too", subprocess, async (context) => {
  const data = await fixture(context, "", "debug");
  let requests = 0;
  const server = await responder(data.socket, (_request, peer) => {
    requests += 1;
    peer.end(encodeAppMessage({ type: "started" }) + encodeAppMessage({ type: "exit", code: 0, signal: null }));
  });
  context.after(() => server.close());
  const result = await data.launch().closed;
  assert.equal(result.code, 0, result.diagnostics);
  assert.equal(requests, 1);
  const calls = (await readFile(data.env.TEST_CARGO_CALLS, "utf8")).trim().split("\n").map((line) => JSON.parse(line) as string[]);
  assert.ok(calls.at(-1)!.some((argument) => argument.startsWith('target."fixture-host".runner=')));
});
