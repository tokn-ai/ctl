import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { buildSignedDevelopmentHelper, type DevelopmentHelperManifest } from "./ctld-signed.mts";
import type { DevelopmentHelperSelection } from "./development-helper.mts";
import type { MacosCommandContext, MacosCommandRunner } from "./macos-provisioning.mts";

const execute = promisify(execFile);
const team = "TEAM123ABC";
const identity = "A".repeat(40);
const revision = "b".repeat(40);
const source_fingerprint = "c".repeat(64);
const target = "aarch64-apple-darwin";
const protocols = [{name:"ctld",build:12,version:"1.0.12",supported_versions:["1.0.12"]}, ...["ctld_lifecycle", "ctld_helper"].map((name) => ({name,build:1,version:"1.0.1",supported_versions:["1.0.1"]}))];

async function fixture(
  t: TestContext,
  failure?: "metadata" | "helper-signing" | "architecture",
  native_options: { native_binary?: boolean; missing_helper_architecture?: boolean; custom_target_directory?: boolean } = {},
) {
  const root = await mkdtemp(join(tmpdir(), "ctld-signed-development-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const target_directory = join(root, native_options.custom_target_directory ? "custom Cargo target" : "target");
  const profile = join(root, "ctld.provisionprofile");
  const ctld = join(root, "cargo-ctld");
  await writeFile(profile, "fixture Personal Team provisioning");
  const helper_bytes = native_options.native_binary
    ? await readFile(process.execPath) : Buffer.from("fixture locally compiled ctld");
  const build_target = native_options.native_binary && process.arch === "x64" ? "x86_64-apple-darwin" : target;
  const architecture = build_target === "aarch64-apple-darwin" ? "arm64" : "x86_64";
  await writeFile(ctld, helper_bytes);
  await mkdir(join(target_directory, "ctl-dev"), { recursive: true, mode: 0o700 });
  const output = join(target_directory, "ctl-dev/ctl");
  await writeFile(output, "previous usable ctl");
  const calls: { command: string; args: string[]; context: MacosCommandContext }[] = [];
  const run: MacosCommandRunner = async (command, args, context) => {
    calls.push({ command, args, context });
    const empty = { stdout: "", stderr: "" };
    if (command === "rustc") return { stdout: `host: ${build_target}\n`, stderr: "" };
    if (command === "cargo" && args[0] === "metadata") {
      return { stdout: JSON.stringify({ target_directory, packages: ["ctld", "ctl-cli"].map((name) => ({ name, version: "0.1.0" })) }), stderr: "" };
    }
    if (command === "security") return empty;
    if (command === "/usr/libexec/PlistBuddy") {
      return { stdout: args[1]?.endsWith(":ExpirationDate") ? "2099-01-01T00:00:00Z" : `${team}.dev.tokn-ai.ctl.ctld`, stderr: "" };
    }
    if (command === "cargo" && args[0] === "build") {
      assert.ok(args.includes("--locked"));
      assert.equal(args[args.indexOf("--target") + 1], build_target);
      const cargo_target = args[args.indexOf("--target-dir") + 1]!;
      assert.equal(cargo_target, join(target_directory, "ctl-dev/cargo"));
      await mkdir(cargo_target, { recursive: true });
      assert.equal(args[args.indexOf("-p") + 1], "ctld");
      assert.equal(context.env.CTL_BUNDLED_CTLD_DIR, undefined);
      assert.equal(context.env.CTL_BUNDLED_CTLD_MODE, undefined);
      return { stdout: JSON.stringify({ reason: "compiler-artifact", target: { name: "ctld", kind: ["bin"] }, executable: ctld }), stderr: "" };
    }
    if (args[0] === "--component-info") {
      assert.notEqual(command, ctld);
      assert.deepEqual(await readFile(command), helper_bytes);
      // Changing Cargo's original output cannot change the snapshot being signed.
      await writeFile(ctld, "concurrent Cargo replacement");
      return { stdout: JSON.stringify({ build: {
        version: "0.1.0", source_revision: revision,
        source_fingerprint: failure === "metadata" ? "malformed" : source_fingerprint, dirty: true,
      }, protocols }), stderr: "" };
    }
    if (command === "/bin/sh") {
      if (failure === "helper-signing") throw new Error("fixture helper signing failed");
      assert.equal(context.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING, "false");
      assert.equal(context.env.CTLD_SIGNING_TIMESTAMP, "none");
      assert.equal(context.env.CTLD_PROVISIONING_PROFILE, profile);
      const app = args[2]!;
      await mkdir(join(app, "Contents/MacOS"), { recursive: true });
      await mkdir(join(app, "Contents/_CodeSignature"));
      await copyFile(args[1]!, join(app, "Contents/MacOS/ctld"));
      await chmod(join(app, "Contents/MacOS/ctld"), 0o755);
      assert.deepEqual(await readFile(join(app, "Contents/MacOS/ctld")), helper_bytes);
      await copyFile(profile, join(app, "Contents/embedded.provisionprofile"));
      await writeFile(join(app, "Contents/Info.plist"), "fixture helper metadata");
      await writeFile(join(app, "Contents/_CodeSignature/CodeResources"), "fixture resource seal");
      await writeFile(context.env.CTLD_SIGNING_IDENTITY_OUTPUT!, identity);
      return empty;
    }
    if (command === "lipo") {
      assert.equal(args.length, 3);
      assert.equal((await lstat(args[0]!)).isFile(), true);
      assert.equal(args[1], "-verify_arch");
      assert.equal(args[2], architecture);
      if (failure === "architecture") throw new Error("fixture wrong architecture");
      if (native_options.native_binary) {
        const verification_args = native_options.missing_helper_architecture
          ? [args[0]!, "-verify_arch", "ppc"] : args;
        return execute("/usr/bin/lipo", verification_args);
      }
      return empty;
    }
    if (command === "codesign") {
      if (args.includes("-d")) return { stdout: "", stderr: `TeamIdentifier=${team}\n` };
      assert.equal(args.includes("--force"), false);
      return empty;
    }
    if (command === "env") return execute(command, args);
    throw new Error(`unexpected fixture command: ${command} ${args.join(" ")}`);
  };
  return {
    root, output, target_directory, calls, run,
    options: {
      repository_root: root, home_directory: join(root, "home"), env: {
        CTLD_PROVISIONING_PROFILE: profile, CTLD_REQUIRE_DISTRIBUTION_SIGNING: "true",
        CTLD_SIGNING_TIMESTAMP: "secure",
        CTL_BUNDLED_CTLD_DIR: "unrelated release payload", CTL_BUNDLED_CTLD_MODE: "release",
      },
    },
  };
}

test("one component build publishes a complete signed ctld for CLI and GUI consumers", async (t) => {
  const input = await fixture(t);
  const executable = await buildSignedDevelopmentHelper(input.options, input.run);
  const checkpoint = await readCheckpoint(input);
  const manifest = checkpoint.receipt;
  assert.equal(executable, checkpoint.executable);
  assert.equal(manifest.signing_mode, "development");
  assert.equal(manifest.notarized, false);
  assert.equal(manifest.bundle_id, `dev.${manifest.sha256}`);
  assert.equal(manifest.git_revision, revision);
  assert.deepEqual(manifest.development, { source_fingerprint, dirty: true });
  assert.deepEqual(manifest.protocols, protocols);
  assert.equal(await readFile(input.output, "utf8"), "previous usable ctl");
  assert.deepEqual((await readdir(join(input.target_directory, "ctl-dev"))).sort(), ["cargo", "ctl", "helpers"]);
  assert.equal(input.calls.some((call) => ["git", "xcrun", "spctl"].includes(call.command)), false);
  assert.equal(input.calls.some((call) => call.context.env.CTLD_SIGNING_TIMESTAMP === "secure"), false);
  assert.equal(input.calls.filter((call) => call.command === "cargo" && call.args[0] === "build").length, 1);
  assert.equal(checkpoint.selection.directory, `build-${manifest.sha256}`);
  assert.deepEqual(await readFile(executable), Buffer.from("fixture locally compiled ctld"));
  const app = join(executable, "../../..");
  assert.equal(await readFile(join(app, "Contents/embedded.provisionprofile"), "utf8"), "fixture Personal Team provisioning");
  assert.equal(await readFile(join(app, "Contents/_CodeSignature/CodeResources"), "utf8"), "fixture resource seal");
});

async function readCheckpoint(input: Awaited<ReturnType<typeof fixture>>) {
  const repository_root = await realpath(input.root);
  const identity = createHash("sha256").update(repository_root, "utf8").digest("hex").slice(0, 20);
  const directory = join(input.target_directory, "ctl-dev/helpers", identity);
  const selected = join(directory, "selected.json");
  const selection: DevelopmentHelperSelection = JSON.parse(await readFile(selected, "utf8"));
  assert.equal(selection.repository_root, repository_root);
  assert.equal(selection.target, target);
  const receipt: DevelopmentHelperManifest = JSON.parse(await readFile(join(directory, selection.directory, "ctld-package.json"), "utf8"));
  return { selected, selection, receipt, executable: join(directory, selection.directory, "ctld.app/Contents/MacOS/ctld") };
}

test("component provisioning honors a custom Cargo target directory without building or signing ctl", async (t) => {
  const input = await fixture(t, undefined, { custom_target_directory: true });
  const helper = await buildSignedDevelopmentHelper(input.options, input.run);
  const checkpoint = await readCheckpoint(input);
  assert.equal(helper, checkpoint.executable);
  assert.equal(checkpoint.receipt.signing_mode, "development");
  assert.equal(await readFile(input.output, "utf8"), "previous usable ctl");
  const packages = input.calls.filter((call) => call.command === "cargo" && call.args[0] === "build")
    .map((call) => call.args[call.args.indexOf("-p") + 1]);
  assert.deepEqual(packages, ["ctld"]);
  assert.equal(input.calls.some((call) => call.command === "codesign" && call.args.includes("--force")), false);
  assert.equal(input.calls.some((call) => call.context.env.CTL_BUNDLED_CTLD_DIR !== undefined), false);
});

test("failed component signing preserves the previous checkout selection", async (t) => {
  const input = await fixture(t);
  await buildSignedDevelopmentHelper(input.options, input.run);
  const checkpoint = await readCheckpoint(input);
  const before = await readFile(checkpoint.selected);
  await writeFile(join(input.root, "cargo-ctld"), "fixture locally compiled ctld");
  const fail_signing: MacosCommandRunner = (command, args, context) => {
    if (command === "/bin/sh") throw new Error("fixture component signing failed");
    return input.run(command, args, context);
  };
  await assert.rejects(buildSignedDevelopmentHelper(input.options, fail_signing), /component signing failed/);
  assert.deepEqual(await readFile(checkpoint.selected), before);
  assert.deepEqual(await readFile(checkpoint.executable), Buffer.from("fixture locally compiled ctld"));
});

for (const failure of ["metadata", "helper-signing", "architecture"] as const) {
  test(`failed ${failure} keeps existing artifacts and cleans unpublished staging`, async (t) => {
    const input = await fixture(t, failure);
    await assert.rejects(buildSignedDevelopmentHelper(input.options, input.run));
    assert.equal(await readFile(input.output, "utf8"), "previous usable ctl");
    assert.deepEqual((await readdir(join(input.target_directory, "ctl-dev"))).sort(), ["cargo", "ctl"]);
  });
}

test("unsupported native targets fail before building or signing", async (t) => {
  const input = await fixture(t);
  const run: MacosCommandRunner = (command, args, context) => command === "rustc"
    ? Promise.resolve({ stdout: "host: x86_64-unknown-linux-gnu\n", stderr: "" })
    : input.run(command, args, context);
  await assert.rejects(buildSignedDevelopmentHelper(input.options, run), /native macOS/);
  assert.equal(input.calls.length, 0);
});

test("overlapping signed builds serialize compilation and signing in their dedicated Cargo cache", async (t) => {
  const input = await fixture(t);
  let release_first!: () => void;
  let started_first!: () => void;
  let visited_second!: () => void;
  const first_started = new Promise<void>((resolve) => { started_first = resolve; });
  const first_gate = new Promise<void>((resolve) => { release_first = resolve; });
  const second_metadata = new Promise<void>((resolve) => { visited_second = resolve; });
  let metadata_calls = 0;
  let helper_builds = 0;
  const run: MacosCommandRunner = async (command, args, context) => {
    if (command === "cargo" && args[0] === "metadata" && ++metadata_calls === 2) visited_second();
    if (command === "cargo" && args[0] === "build" && args[args.indexOf("-p") + 1] === "ctld") {
      helper_builds += 1;
      if (helper_builds === 1) {
        started_first();
        await first_gate;
      }
      // Model Cargo updating the same dedicated artifact during the second build.
      await writeFile(join(input.root, "cargo-ctld"), "fixture locally compiled ctld");
    }
    return input.run(command, args, context);
  };
  const first = buildSignedDevelopmentHelper(input.options, run);
  await first_started;
  const second = buildSignedDevelopmentHelper(input.options, run);
  await second_metadata;
  assert.equal(helper_builds, 1);
  release_first();
  const [first_output, second_output] = await Promise.all([first, second]);
  // Archives include file timestamps, so separate valid builds can have
  // different hashes. The last build is selected and both remain usable.
  assert.equal(second_output, (await readCheckpoint(input)).executable);
  assert.equal(await readFile(first_output, "utf8"), "fixture locally compiled ctld");
  assert.equal(await readFile(second_output, "utf8"), "fixture locally compiled ctld");
  assert.equal(helper_builds, 2);
  const packages = input.calls.filter((call) => call.command === "cargo" && call.args[0] === "build")
    .map((call) => call.args[call.args.indexOf("-p") + 1]);
  assert.deepEqual(packages, ["ctld", "ctld"]);
  assert.deepEqual((await readdir(join(input.target_directory, "ctl-dev"))).sort(), ["cargo", "ctl", "helpers"]);
});

const native_macos = { skip: process.platform !== "darwin" || !["arm64", "x64"].includes(process.arch) };

test("component workflow verifies published Mach-O artifacts with the native lipo parser", native_macos, async (t) => {
  const input = await fixture(t, undefined, { native_binary: true });
  const executable = await buildSignedDevelopmentHelper(input.options, input.run);
  assert.deepEqual(await readFile(executable), await readFile(process.execPath));
  const checks = input.calls.filter((call) => call.command === "lipo");
  assert.equal(checks.length, 2);
  assert.ok(checks[0]!.args[0]!.endsWith("/ctld.app/Contents/MacOS/ctld"));
  assert.ok(checks[1]!.args[0]!.endsWith("/ctld.app/Contents/MacOS/ctld"));
});

test("a native lipo architecture rejection leaves the previous component selection unchanged", native_macos, async (t) => {
  const input = await fixture(t, undefined, { native_binary: true, missing_helper_architecture: true });
  await assert.rejects(buildSignedDevelopmentHelper(input.options, input.run), /lipo/);
  assert.equal(await readFile(input.output, "utf8"), "previous usable ctl");
  assert.equal(input.calls.filter((call) => call.command === "lipo").length, 1);
  assert.equal(input.calls.some((call) => call.command === "codesign" && call.args.includes("--force")), false);
  assert.deepEqual((await readdir(join(input.target_directory, "ctl-dev"))).sort(), ["cargo", "ctl"]);
});
