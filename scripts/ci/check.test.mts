import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";

function fixture(t: TestContext, fail_formatting = false) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ctl-check-test-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts/ci"), { recursive: true });
  mkdirSync(join(root, "bin"));
  copyFileSync(new URL("./check.mts", import.meta.url), join(root, "scripts/ci/check.mts"));
  writeFileSync(join(root, "rust-toolchain.toml"), '[toolchain]\nchannel = "1.99.0"\n');
  const log = join(root, "calls.jsonl");
  writeFileSync(join(root, "scripts/ci/verify-cargo-packages.mts"), `
import { appendFileSync } from "node:fs";
appendFileSync(process.env.CHECK_LOG, JSON.stringify({
  args: process.argv.slice(2), toolchain: process.env.RUSTUP_TOOLCHAIN,
}) + "\\n");
`);
  const cargo = join(root, "bin/cargo");
  writeFileSync(cargo, `#!/usr/bin/env node
const fs = require("node:fs");
fs.appendFileSync(process.env.CHECK_LOG, JSON.stringify({
  args: process.argv.slice(2), cwd: process.cwd(), toolchain: process.env.RUSTUP_TOOLCHAIN,
}) + "\\n");
process.exit(process.env.FAIL_FORMATTING === "yes" && process.argv[2] === "fmt" ? 17 : 0);
`);
  chmodSync(cargo, 0o700);
  return {
    root,
    run(scope: string, ...options: string[]) {
      return spawnSync(process.execPath, [join(root, "scripts/ci/check.mts"), scope, ...options], {
        cwd: tmpdir(), encoding: "utf8",
        env: { ...process.env, PATH: `${join(root, "bin")}:${process.env.PATH}`,
          CHECK_LOG: log, FAIL_FORMATTING: fail_formatting ? "yes" : "no", RUSTUP_TOOLCHAIN: "wrong-default" },
      });
    },
    calls() {
      return readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line));
    },
  };
}

test("checks use the repository root and pinned compiler despite caller defaults", { skip: process.platform === "win32" }, (t) => {
  const f = fixture(t);
  const result = f.run("quality");
  assert.equal(result.status, 0, result.stderr);
  const calls = f.calls();
  assert.equal(calls.length, 2);
  for (const call of calls) {
    assert.equal(call.cwd, f.root);
    assert.equal(call.toolchain, "1.99.0");
  }
  assert.ok(calls[1].args.includes("--locked"));
});

test("a failed check stops later checks and returns failure", { skip: process.platform === "win32" }, (t) => {
  const f = fixture(t, true);
  const result = f.run("quality");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /cargo failed \(17\)/);
  assert.equal(f.calls().length, 1);
});

test("published-source checks retain the minimum Rust version and explicit dirty option", { skip: process.platform === "win32" }, (t) => {
  const f = fixture(t);
  const result = f.run("packages", "--allow-dirty");
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(f.calls(), [{ args: ["--allow-dirty"], toolchain: "1.97.0" }]);
});

test("portable quality and runtime checks cover the same clients, targets, and libraries", { skip: process.platform === "win32" }, (t) => {
  const f = fixture(t);
  for (const scope of ["quality", "rust"]) {
    const result = f.run(scope, "--portable");
    assert.equal(result.status, 0, result.stderr);
  }
  const [formatting, clippy, tests] = f.calls();
  assert.deepEqual(formatting.args, ["fmt", "--all", "--", "--check"]);
  assert.equal(clippy.args[0], "clippy");
  assert.ok(clippy.args.includes("--all-targets"));
  assert.deepEqual(clippy.args.slice(-3), ["--", "-D", "warnings"]);
  assert.equal(tests.args[0], "test");
  assert.ok(tests.args.includes("--no-fail-fast"));
  const packages = (args: string[]): string[] => args.flatMap((value, index) => value === "-p" ? [args[index + 1]] : []);
  assert.deepEqual(packages(clippy.args), packages(tests.args));
  for (const call of [clippy, tests]) {
    assert.ok(call.args.includes("--locked"));
    assert.ok(!call.args.includes("--workspace"));
    for (const name of ["ctl-client", "ctl-agent", "ctl-ipc", "ctl-task-client", "ctmux-client", "ctmux-core", "ctmuxd", "ctmux-avt", "ctmux-process-info"]) {
      assert.ok(packages(call.args).includes(name), `missing portable package ${name}`);
    }
    for (const name of ["ctld", "ctmux-app", "ctl-keychain-client"]) {
      assert.ok(!packages(call.args).includes(name), `unexpected platform package ${name}`);
    }
  }
});

test("portable selection rejects unrelated scopes and packaging options", { skip: process.platform === "win32" }, (t) => {
  const f = fixture(t);
  for (const [scope, ...options] of [
    ["scripts", "--portable"], ["packages", "--portable"], ["all", "--portable"],
    ["quality", "--portable", "--allow-dirty"],
  ]) {
    const result = f.run(scope, ...options);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /usage: node scripts\/ci\/check.mts/);
  }
});
