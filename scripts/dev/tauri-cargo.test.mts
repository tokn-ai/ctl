import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { writeDevelopmentManager } from "./ctlup-development.fixtures.mts";

const runner = fileURLToPath(new URL("./tauri-cargo.sh", import.meta.url));

async function fixture() {
  const directory = await mkdtemp(path.join(tmpdir(), "ctmux-runner-"));
  const log = path.join(directory, "calls.jsonl");
  const manager = await writeDevelopmentManager(directory);
  await writeFile(path.join(directory, "rustc"), `#!${process.execPath}\nconsole.log("host: fixture-host");\n`, { mode: 0o755 });
  await writeFile(path.join(directory, "cargo"), `#!/usr/bin/env node
const fs = require("node:fs");
const args = process.argv.slice(2);
fs.appendFileSync(process.env.FIXTURE_LOG, JSON.stringify({ args, pid: process.pid, ctld:process.env.CTLD_BIN, ctmuxd:process.env.CTMUXD_BIN, installation_id:process.env.CTL_DEV_INSTALLATION_ID }) + "\\n");
if (args[0] === "metadata") console.log(JSON.stringify({workspace_root:process.env.FIXTURE_WORKSPACE}));
if (args[0] === "build") {
  if (process.env.FIXTURE_BUILD_ERROR) {
    console.error(process.env.FIXTURE_BUILD_ERROR);
    process.exit(101);
  }
  console.log(JSON.stringify({reason: "compiler-artifact", target: {name:"ctlup", kind: ["bin"]}, executable:process.env.FIXTURE_MANAGER}));
}
`, { mode: 0o755 });
  return {
    directory,
    installation: manager.installation,
    env: {
      ...process.env,
      CTMUX_DEV_APP_SUPERVISOR: undefined,
      PATH: `${directory}${path.delimiter}${process.env.PATH}`,
      FIXTURE_LOG: log,
      FIXTURE_WORKSPACE: directory,
      FIXTURE_MANAGER: manager.manager_executable,
    },
    calls: async () => (await readFile(log, "utf8")).trim().split("\n").map((line) => JSON.parse(line) as { args: string[]; pid: number; ctld?: string; ctmuxd?: string; installation_id?: string }),
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

test("unsigned native launch prepares a managed cohort before running Cargo", { skip: process.platform === "win32" }, async () => {
  const context = await fixture();
  try {
    const args = ["run", "--target-dir", "custom output", "--", "app argument"];
    const first = await launch(args, context.env);
    const second = await launch(args, context.env);
    assert.equal(first.code, 0, first.stderr);
    assert.equal(second.code, 0, second.stderr);
    const calls = await context.calls();
    assert.deepEqual(calls.map((call) => call.args[0]), ["metadata", "build", "run", "metadata", "build", "run"]);
    assert.deepEqual(calls[2]!.args, args);
    assert.equal(calls[2]!.ctld, context.installation.executables.ctld);
    assert.equal(calls[2]!.ctmuxd, context.installation.executables.ctmuxd);
    assert.equal(calls[2]!.installation_id, context.installation.bundle_id);
  } finally {
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
    assert.deepEqual((await context.calls()).map((call) => call.args[0]), ["metadata", "build"]);

    const retried = await launch(["run"], context.env);
    assert.equal(retried.code, 0, retried.stderr);
    assert.deepEqual((await context.calls()).map((call) => call.args[0]), ["metadata", "build", "metadata", "build", "run"]);
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
    assert.deepEqual((await context.calls()).map((call) => call.args[0]), ["metadata", "build"]);
  } finally {
    await context.close();
  }
});

test("release builds pass directly through the runner", { skip: process.platform === "win32" }, async () => {
  const context = await fixture();
  try {
    const result = await launch(["build", "--release"], context.env);
    assert.equal(result.code, 0, result.stderr);
    const calls = await context.calls();
    assert.equal(calls.length, 1);
    assert.deepEqual(calls[0]!.args, ["build", "--release"]);
    assert.equal(calls[0]!.pid, result.pid);
  } finally {
    await context.close();
  }
});
