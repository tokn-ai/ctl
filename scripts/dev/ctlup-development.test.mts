import assert from "node:assert/strict";
import { access, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test, { type TestContext } from "node:test";
import { developmentEnvironment, parseDevelopmentInstallation, prepareDevelopment } from "./ctlup-development.mts";
import { writeDevelopmentManager } from "./ctlup-development.fixtures.mts";

async function fixture(context: TestContext) {
  const directory = await realpath(await mkdtemp(path.join(tmpdir(), "ctlup-development-")));
  const manager = await writeDevelopmentManager(directory);
  const cargo_log = path.join(directory, "cargo.jsonl");
  const manager_log = path.join(directory, "manager.jsonl");
  await writeFile(path.join(directory, "rustc"), `#!${process.execPath}\nconsole.log("host: fixture-host");\n`, { mode: 0o755 });
  await writeFile(path.join(directory, "cargo"), `#!${process.execPath}
const fs = require("node:fs");
const args = process.argv.slice(2);
fs.appendFileSync(process.env.TEST_CARGO_CALLS, JSON.stringify({args,cwd:process.cwd()})+"\\n");
if(args[0] === "metadata") console.log(JSON.stringify({workspace_root:process.env.TEST_WORKSPACE}));
else if(args[0] === "build") console.log(JSON.stringify({reason:"compiler-artifact",target:{name:"ctlup",kind:["bin"]},executable:process.env.TEST_MANAGER_EXECUTABLE}));
else { console.error("recursive cargo invocation"); process.exit(1); }
`, { mode: 0o755 });
  const env: NodeJS.ProcessEnv = {
    PATH: [directory, path.dirname(process.execPath), "/usr/bin", "/bin"].join(path.delimiter),
    TEST_WORKSPACE: directory,
    TEST_CARGO_CALLS: cargo_log,
    TEST_MANAGER_CALLS: manager_log,
    TEST_MANAGER_EXECUTABLE: manager.manager_executable,
  };
  context.after(() => rm(directory, { recursive: true, force: true }));
  return { directory, manager, cargo_log, manager_log, env };
}

const subprocess = { skip: process.platform === "win32" };

test("bootstrap builds only the native manager, preserving original Cargo settings for ctlup", subprocess, async (context) => {
  const data = await fixture(context);
  const cargo_args = ["run", "--locked", "--profile", "custom", "--target-dir", "custom output", "--config", "relative.toml", "--target", "foreign-target", "--", "app argument"];
  const prepared = await prepareDevelopment(cargo_args, data.directory, data.env);
  let exited = false;
  void prepared.exited.then(() => { exited = true; });
  try {
    assert.deepEqual(prepared.installation, data.manager.installation);
    const calls = (await readFile(data.cargo_log, "utf8")).trim().split("\n").map((line) => JSON.parse(line) as { args: string[]; cwd: string });
    assert.deepEqual(calls.map((call) => call.args[0]), ["metadata", "build"]);
    const build = calls[1]!.args;
    assert.equal(build.filter((argument) => argument === "--locked").length, 1);
    assert.equal(build[build.indexOf("--target") + 1], "fixture-host");
    assert.equal(build[build.indexOf("--target-dir") + 1], "custom output");
    assert.equal(build[build.indexOf("--config") + 1], "relative.toml");
    assert.equal(build[build.indexOf("--bin") + 1], "ctlup");
    const manager_call = JSON.parse((await readFile(data.manager_log, "utf8")).trim()) as { args: string[]; cwd: string; executable: string };
    assert.deepEqual(JSON.parse(manager_call.args[manager_call.args.indexOf("--cargo-args-json") + 1]!), cargo_args);
    assert.equal(manager_call.cwd, data.directory);
    assert.ok(manager_call.args.includes("--lease"));
    assert.notEqual(manager_call.executable, data.manager.manager_executable);
    await access(manager_call.executable);
    assert.equal(exited, false);
    await prepared.close();
    await prepared.exited;
    await assert.rejects(access(manager_call.executable), { code: "ENOENT" });
  } finally {
    await prepared.close();
  }
});

test("immutable daemon paths override caller overrides and the GUI receives the cohort lease identity", subprocess, async (context) => {
  const data = await fixture(context);
  const installation = data.manager.installation;
  const env = developmentEnvironment(installation, { CTLD_BIN: "/synthetic/old", CTMUXD_BIN: "/synthetic/old", CTL_TASKD_BIN: "/synthetic/old", OTHER: "retained" });
  assert.equal(env.CTLD_BIN, installation.executables.ctld);
  assert.equal(env.CTMUXD_BIN, installation.executables.ctmuxd);
  assert.equal(env.CTL_TASKD_BIN, installation.executables["ctl-taskd"]);
  assert.equal(env.CTL_DEV_INSTALLATION_ID, installation.bundle_id);
  assert.equal(env.OTHER, "retained");
  assert.throws(() => parseDevelopmentInstallation({ ...installation, executables: { ctl: "/synthetic/ctl" } }), /complete native/);
});

test("failed manager preparation reports the failure without running an app Cargo command", subprocess, async (context) => {
  const data = await fixture(context);
  await assert.rejects(prepareDevelopment(["run"], data.directory, { ...data.env, TEST_MANAGER_FAILURE: "synthetic activation failure" }), /synthetic activation failure/);
  const calls = (await readFile(data.cargo_log, "utf8")).trim().split("\n").map((line) => JSON.parse(line).args[0]);
  assert.deepEqual(calls, ["metadata", "build"]);
});
