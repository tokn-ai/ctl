import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { requestPreparation, servePreparation } from "./daemon-preparation.mts";

const runner = fileURLToPath(new URL("./tauri-cargo.sh", import.meta.url));

async function fixture() {
  const directory = await mkdtemp(path.join(tmpdir(), "rmux-runner-"));
  const log = path.join(directory, "calls.jsonl");
  const artifacts = Object.fromEntries(["ctld", "rmuxd", "taskd"].map((name) => [name, path.join(directory, name)]));
  for (const artifact of Object.values(artifacts)) await writeFile(artifact, "fixture");
  await writeFile(path.join(directory, "cargo"), `#!/usr/bin/env node
const fs = require("node:fs");
const args = process.argv.slice(2);
fs.appendFileSync(process.env.FIXTURE_LOG, JSON.stringify({ args, pid: process.pid }) + "\\n");
if (args[0] === "build") {
  if (process.env.FIXTURE_BUILD_ERROR) {
    console.error(process.env.FIXTURE_BUILD_ERROR);
    process.exit(101);
  }
  for (const [name, executable] of Object.entries(JSON.parse(process.env.FIXTURE_ARTIFACTS))) {
    console.log(JSON.stringify({reason: "compiler-artifact", target: {name, kind: ["bin"]}, executable}));
  }
}
`, { mode: 0o755 });
  return {
    directory,
    artifacts,
    env: {
      ...process.env,
      PATH: `${directory}${path.delimiter}${process.env.PATH}`,
      FIXTURE_LOG: log,
      FIXTURE_ARTIFACTS: JSON.stringify(artifacts),
    },
    calls: async () => (await readFile(log, "utf8")).trim().split("\n").map((line) => JSON.parse(line) as { args: string[]; pid: number }),
    close: () => rm(directory, { recursive: true, force: true }),
  };
}

async function launch(args: string[], env: NodeJS.ProcessEnv) {
  const child = spawn(runner, args, { env, stdio: ["ignore", "pipe", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (data) => { stderr += String(data); });
  child.stdout.resume();
  const code = await new Promise<number | null>((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", resolve);
  });
  return { pid: child.pid, code, stderr };
}

test("each native launch prepares current helpers before handing its PID to Cargo", { skip: process.platform === "win32" }, async () => {
  const context = await fixture();
  const socket = path.join(context.directory, "prepare.sock");
  let preparations = 0;
  const server = await servePreparation(socket, async (executable) => {
    assert.equal(executable, context.artifacts.ctld);
    // Cargo run must not start while signing/readiness is pending.
    const calls = await context.calls();
    assert.equal(calls.at(-1)?.args[0], "build");
    preparations += 1;
  });
  try {
    const args = ["run", "--target-dir", "custom output", "--", "app argument"];
    const env = { ...context.env, RMUX_DEV_DAEMON_SUPERVISOR: socket };
    const first = await launch(args, env);
    const second = await launch(args, env);
    assert.equal(first.code, 0, first.stderr);
    assert.equal(second.code, 0, second.stderr);
    assert.equal(preparations, 2);
    const calls = await context.calls();
    assert.deepEqual(calls.map((call) => call.args[0]), ["build", "run", "build", "run"]);
    assert.equal(calls[1].pid, first.pid);
    assert.equal(calls[3].pid, second.pid);
    assert.deepEqual(calls[1].args, args);
  } finally {
    await server.close();
    await context.close();
  }
});

test("a signing failure prevents the new client from launching", { skip: process.platform === "win32" }, async () => {
  const context = await fixture();
  const socket = path.join(context.directory, "prepare.sock");
  const server = await servePreparation(socket, async () => { throw new Error("signing failed"); });
  try {
    const result = await launch(["run"], { ...context.env, RMUX_DEV_DAEMON_SUPERVISOR: socket });
    assert.equal(result.code, 1);
    assert.match(result.stderr, /signing failed/);
    assert.deepEqual((await context.calls()).map((call) => call.args[0]), ["build"]);
  } finally {
    await server.close();
    await context.close();
  }
});

test("compiler errors preserve Tauri's recoverable failure status and final diagnostic", { skip: process.platform === "win32" }, async () => {
  const context = await fixture();
  try {
    const result = await launch(["run"], {
      ...context.env,
      FIXTURE_BUILD_ERROR: "error: could not compile `ctld` (bin) due to 1 previous error",
    });
    assert.equal(result.code, 101);
    assert.match(result.stderr.trim().split("\n").at(-1) ?? "", /could not compile/);
    assert.deepEqual((await context.calls()).map((call) => call.args[0]), ["build"]);

    const retried = await launch(["run"], context.env);
    assert.equal(retried.code, 0, retried.stderr);
    assert.deepEqual((await context.calls()).map((call) => call.args[0]), ["build", "build", "run"]);
  } finally {
    await context.close();
  }
});

test("other Cargo errors are not labeled recoverable compiler failures", { skip: process.platform === "win32" }, async () => {
  const context = await fixture();
  try {
    const result = await launch(["run"], {
      ...context.env,
      FIXTURE_BUILD_ERROR: "error: the lock file needs to be updated but --locked was passed",
    });
    assert.equal(result.code, 1);
    assert.doesNotMatch(result.stderr, /could not compile/);
    assert.deepEqual((await context.calls()).map((call) => call.args[0]), ["build"]);
  } finally {
    await context.close();
  }
});

test("release builds pass directly through the runner", { skip: process.platform === "win32" }, async () => {
  const context = await fixture();
  try {
    const result = await launch(["build", "--release"], context.env);
    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(await context.calls(), [{ args: ["build", "--release"], pid: result.pid }]);
  } finally {
    await context.close();
  }
});

test("a missing signed supervisor fails instead of launching an unprepared client", async () => {
  await assert.rejects(requestPreparation(path.join(tmpdir(), "rmux-no-such-supervisor", "prepare.sock"), "ctld"), /ENOENT/);
});
