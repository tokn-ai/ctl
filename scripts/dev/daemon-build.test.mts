import assert from "node:assert/strict";
import { mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import {
  buildDaemons,
  DaemonBuildError,
  parseDaemonArtifact,
  selectDaemonBuildArguments,
} from "./daemon-build.mts";

test("selects Cargo settings while excluding app targets, features, and arguments", () => {
  assert.deepEqual(selectDaemonBuildArguments([
    "run", "--package", "rmux-app", "--features", "custom-protocol",
    "--bin=rmux-app", "--no-default-features", "--all-features",
    "--message-format", "json", "--target", "aarch64-apple-darwin",
    "--target-dir", "/tmp/custom target", "--profile=development",
    "--manifest-path", "apps/desktop/src-tauri/Cargo.toml",
    "--locked", "--offline", "--config", "build.incremental=false",
    "--color", "always", "--", "--target-dir", "/tmp/app-only",
  ]), [
    "--target", "aarch64-apple-darwin", "--target-dir", "/tmp/custom target",
    "--profile=development", "--manifest-path", "apps/desktop/src-tauri/Cargo.toml",
    "--locked", "--offline", "--config", "build.incremental=false",
    "--color", "always",
  ]);
});

test("preserves short Cargo options and equals-form build settings", () => {
  assert.deepEqual(selectDaemonBuildArguments([
    "run", "-p", "rmux-app", "-Fcustom-protocol", "-F", "devtools",
    "-r", "-vv", "-j", "4", "-j2", "-mCargo.toml", "-Z", "unstable-options",
    "--target=aarch64-apple-darwin", "--target-dir=custom", "--frozen",
    "--ignore-rust-version", "--keep-going", "--future-incompat-report",
    "--timings=json",
  ]), [
    "-r", "-vv", "-j", "4", "-j2", "-mCargo.toml", "-Z", "unstable-options",
    "--target=aarch64-apple-darwin", "--target-dir=custom", "--frozen",
    "--ignore-rust-version", "--keep-going", "--future-incompat-report",
    "--timings=json",
  ]);
  assert.throws(() => selectDaemonBuildArguments(["--target"]), /requires a value/);
  assert.throws(() => selectDaemonBuildArguments(["-j", "--"]), /requires a value/);
});

function artifact(name: string, executable: string | null, extra = {}): string {
  return JSON.stringify({
    reason: "compiler-artifact",
    target: { name, kind: ["bin"] },
    executable,
    fresh: true,
    ...extra,
  });
}

test("reads fresh daemon artifacts without inferring executable paths", () => {
  assert.deepEqual(parseDaemonArtifact(artifact("ctld", "/custom/dev/ctld")), {
    daemon: "ctld",
    executable: "/custom/dev/ctld",
  });
  for (const line of [
    "build script output",
    "null",
    "{}",
    '{"reason":"build-finished","success":true}',
    artifact("dependency", "/custom/dependency"),
    artifact("ctld", null),
    artifact("ctld", "/custom/lib", { target: { name: "ctld", kind: ["lib"] } }),
    artifact("ctld", "/custom/test", { profile: { test: true } }),
  ]) {
    assert.equal(parseDaemonArtifact(line), undefined);
  }
});

async function fakeCargo(
  context: TestContext,
  output: string[],
  status = 0,
  diagnostics = "",
): Promise<{ cwd: string; env: NodeJS.ProcessEnv; invocation: string }> {
  const cwd = await realpath(await mkdtemp(join(tmpdir(), "rmux-daemon-build-test-")));
  context.after(() => rm(cwd, { recursive: true, force: true }));
  const invocation = join(cwd, "cargo-invocation.json");
  await writeFile(join(cwd, "cargo"), `#!${process.execPath}
const fs = require("node:fs");
fs.writeFileSync(process.env.TEST_CARGO_INVOCATION, JSON.stringify({
  args: process.argv.slice(2), cwd: process.cwd(), target_dir: process.env.CARGO_TARGET_DIR,
}));
process.stdout.write(${JSON.stringify(output.join("\n") + "\n")});
process.stderr.write(${JSON.stringify(diagnostics)});
process.exitCode = ${status};
`, { mode: 0o755 });
  return {
    cwd,
    invocation,
    env: {
      ...process.env,
      PATH: [cwd, dirname(process.execPath), process.env.PATH].join(delimiter),
      TEST_CARGO_INVOCATION: invocation,
      CARGO_TARGET_DIR: "custom target directory",
    },
  };
}

const subprocessOptions = { skip: process.platform === "win32" };

test("builds all daemon packages and returns Cargo-reported custom output paths", subprocessOptions, async (context) => {
  const cargo = await fakeCargo(context, [
    artifact("ctld", "custom target/aarch64-apple-darwin/development/ctld"),
    artifact("rmuxd", "/different-output/rmuxd"),
    artifact("taskd", "custom target/aarch64-apple-darwin/development/taskd"),
  ]);
  assert.deepEqual(await buildDaemons([
    "run", "--profile", "development", "--target", "aarch64-apple-darwin",
    "--features", "app-only", "--locked", "--", "--app-only",
  ], cargo.cwd, cargo.env), {
    ctld: join(cargo.cwd, "custom target/aarch64-apple-darwin/development/ctld"),
    rmuxd: "/different-output/rmuxd",
    taskd: join(cargo.cwd, "custom target/aarch64-apple-darwin/development/taskd"),
  });
  assert.deepEqual(JSON.parse(await readFile(cargo.invocation, "utf8")), {
    args: [
      "build", "--profile", "development", "--target", "aarch64-apple-darwin", "--locked",
      "--message-format=json-render-diagnostics",
      "--package", "ctld", "--package", "rmuxd", "--package", "taskd",
    ],
    cwd: cargo.cwd,
    target_dir: "custom target directory",
  });
});

test("rejects failed builds even when Cargo emitted all executable artifacts", subprocessOptions, async (context) => {
  const cargo = await fakeCargo(context, [
    artifact("ctld", "/target/ctld"),
    artifact("rmuxd", "/target/rmuxd"),
    artifact("taskd", "/target/taskd"),
  ], 101);
  await assert.rejects(buildDaemons([], cargo.cwd, cargo.env), (error: unknown) => {
    assert.ok(error instanceof DaemonBuildError);
    assert.equal(error.exit_code, 101);
    assert.equal(error.compilation_failed, false);
    return true;
  });
});

test("identifies compiler failures separately from other Cargo errors", subprocessOptions, async (context) => {
  const cargo = await fakeCargo(context, [], 101, "error: could not compile `ctld` (bin) due to 1 previous error\n");
  await assert.rejects(buildDaemons([], cargo.cwd, cargo.env), (error: unknown) => {
    assert.ok(error instanceof DaemonBuildError);
    assert.equal(error.exit_code, 101);
    assert.equal(error.signal, null);
    assert.equal(error.compilation_failed, true);
    return true;
  });
});

test("rejects incomplete successful builds instead of guessing old executable paths", subprocessOptions, async (context) => {
  const cargo = await fakeCargo(context, [artifact("ctld", "/target/ctld")]);
  await assert.rejects(buildDaemons([], cargo.cwd, cargo.env), /executable for rmuxd/);
});

test("rejects ambiguous daemon artifacts from multiple Cargo targets", subprocessOptions, async (context) => {
  const cargo = await fakeCargo(context, [
    artifact("ctld", "/target/first/ctld"),
    artifact("ctld", "/target/second/ctld"),
  ]);
  await assert.rejects(buildDaemons([], cargo.cwd, cargo.env), /multiple executables for ctld/);
});

test("propagates Cargo spawn failures", subprocessOptions, async (context) => {
  const cargo = await fakeCargo(context, []);
  await assert.rejects(buildDaemons([], cargo.cwd, {
    ...cargo.env,
    PATH: join(cargo.cwd, "missing"),
  }), /ENOENT/);
});
