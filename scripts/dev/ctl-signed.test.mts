import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, copyFile, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { buildSignedDevelopmentCli, type DevelopmentHelperManifest } from "./ctl-signed.mts";
import type { MacosCommandContext, MacosCommandRunner } from "./macos-provisioning.mts";

const execute = promisify(execFile);
const team = "TEAM123ABC";
const identity = "A".repeat(40);
const revision = "b".repeat(40);
const source_fingerprint = "c".repeat(64);
const target = "aarch64-apple-darwin";
const signature = "\nfixture Apple Development signature";

async function fixture(t: TestContext, failure?: "metadata" | "helper-signing" | "cli-signing" | "cli-building" | "architecture") {
  const root = await mkdtemp(join(tmpdir(), "ctl-signed-development-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const target_directory = join(root, "target");
  const profile = join(root, "ctld.provisionprofile");
  const ctld = join(root, "cargo-ctld");
  const ctl = join(root, "cargo-ctl");
  await writeFile(profile, "fixture Personal Team provisioning");
  await writeFile(ctld, "fixture locally compiled ctld");
  await mkdir(join(target_directory, "ctl-dev"), { recursive: true, mode: 0o700 });
  const output = join(target_directory, "ctl-dev/ctl");
  await writeFile(output, "previous usable ctl");
  const calls: { command: string; args: string[]; context: MacosCommandContext }[] = [];
  let manifest: DevelopmentHelperManifest | undefined;
  const run: MacosCommandRunner = async (command, args, context) => {
    calls.push({ command, args, context });
    const empty = { stdout: "", stderr: "" };
    if (command === "rustc") return { stdout: `host: ${target}\n`, stderr: "" };
    if (command === "cargo" && args[0] === "metadata") {
      return { stdout: JSON.stringify({ target_directory, packages: ["ctld", "ctl-cli"].map((name) => ({ name, version: "0.1.0" })) }), stderr: "" };
    }
    if (command === "security") return empty;
    if (command === "/usr/libexec/PlistBuddy") {
      return { stdout: args[1]?.endsWith(":ExpirationDate") ? "2099-01-01T00:00:00Z" : `${team}.dev.tokn-ai.ctl.ctld`, stderr: "" };
    }
    if (command === "cargo" && args[0] === "build") {
      assert.ok(args.includes("--locked"));
      assert.equal(args[args.indexOf("--target") + 1], target);
      const cargo_target = args[args.indexOf("--target-dir") + 1]!;
      assert.equal(cargo_target, join(target_directory, "ctl-dev/cargo"));
      await mkdir(cargo_target, { recursive: true });
      const name = args[args.indexOf("-p") + 1];
      if (name === "ctld") {
        assert.equal(context.env.CTL_BUNDLED_CTLD_DIR, undefined);
        assert.equal(context.env.CTL_BUNDLED_CTLD_MODE, undefined);
        return { stdout: JSON.stringify({ reason: "compiler-artifact", target: { name: "ctld", kind: ["bin"] }, executable: ctld }), stderr: "" };
      }
      if (failure === "cli-building") throw new Error("fixture CLI build failed");
      assert.equal(context.env.CTL_BUNDLED_CTLD_MODE, "development");
      const payload = context.env.CTL_BUNDLED_CTLD_DIR!;
      manifest = JSON.parse(await readFile(join(payload, `ctld-${target}.json`), "utf8"));
      const bytes = await readFile(join(payload, manifest!.archive));
      assert.equal(manifest!.sha256, createHash("sha256").update(bytes).digest("hex"));
      const transported = join(root, "transported");
      await mkdir(transported, { recursive: true });
      await execute("tar", ["-xzf", join(payload, manifest!.archive), "-C", transported]);
      assert.equal(await readFile(join(transported, "ctld.app/Contents/embedded.provisionprofile"), "utf8"), "fixture Personal Team provisioning");
      assert.equal(await readFile(join(transported, "ctld.app/Contents/_CodeSignature/CodeResources"), "utf8"), "fixture resource seal");
      assert.equal((await readdir(join(transported, "ctld.app/Contents"))).includes("CodeResources"), false);
      await writeFile(ctl, Buffer.concat([Buffer.from("fixture self-contained ctl\n"), bytes]));
      return { stdout: JSON.stringify({ reason: "compiler-artifact", target: { name: "ctl", kind: ["bin"] }, executable: ctl }), stderr: "" };
    }
    if (args[0] === "--component-info") {
      assert.notEqual(command, ctld);
      assert.equal(await readFile(command, "utf8"), "fixture locally compiled ctld");
      // Changing Cargo's original output cannot change the snapshot being signed.
      await writeFile(ctld, "concurrent Cargo replacement");
      return { stdout: JSON.stringify({ build: {
        version: "0.1.0", source_revision: revision,
        source_fingerprint: failure === "metadata" ? "malformed" : source_fingerprint, dirty: true,
      }, protocols: [] }), stderr: "" };
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
      assert.equal(await readFile(join(app, "Contents/MacOS/ctld"), "utf8"), "fixture locally compiled ctld");
      await copyFile(profile, join(app, "Contents/embedded.provisionprofile"));
      await writeFile(join(app, "Contents/Info.plist"), "fixture helper metadata");
      await writeFile(join(app, "Contents/_CodeSignature/CodeResources"), "fixture resource seal");
      await writeFile(context.env.CTLD_SIGNING_IDENTITY_OUTPUT!, identity);
      return empty;
    }
    if (command === "lipo") {
      assert.equal(args[1], "arm64");
      if (failure === "architecture") throw new Error("fixture wrong architecture");
      return empty;
    }
    if (command === "codesign") {
      if (args.includes("-d")) return { stdout: "", stderr: `TeamIdentifier=${team}\n` };
      if (args.includes("--force")) {
        if (failure === "cli-signing") throw new Error("fixture CLI signing failed");
        assert.equal(args[args.indexOf("--sign") + 1], identity);
        assert.ok(args.includes("--timestamp=none"));
        assert.equal(args.includes("--timestamp"), false);
        assert.equal(args[args.indexOf("--identifier") + 1], "dev.tokn-ai.ctl.cli");
        await writeFile(args.at(-1)!, Buffer.concat([await readFile(args.at(-1)!), Buffer.from(signature)]));
      }
      return empty;
    }
    if (command === "env") return execute(command, args);
    throw new Error(`unexpected fixture command: ${command} ${args.join(" ")}`);
  };
  return {
    root, output, target_directory, calls, run, get_manifest: () => manifest,
    options: {
      repository_root: root, home_directory: join(root, "home"), env: {
        CTLD_PROVISIONING_PROFILE: profile, CTLD_REQUIRE_DISTRIBUTION_SIGNING: "true",
        CTLD_SIGNING_TIMESTAMP: "secure",
        CTL_BUNDLED_CTLD_DIR: "unrelated release payload", CTL_BUNDLED_CTLD_MODE: "release",
      },
    },
  };
}

test("one native development build embeds a complete locally signed helper without release credentials", async (t) => {
  const input = await fixture(t);
  assert.equal(await buildSignedDevelopmentCli(input.options, input.run), input.output);
  const manifest = input.get_manifest()!;
  assert.equal(manifest.signing_mode, "development");
  assert.equal(manifest.notarized, false);
  assert.equal(manifest.bundle_id, `dev.${manifest.sha256}`);
  assert.equal(manifest.git_revision, revision);
  assert.deepEqual(manifest.development, { source_fingerprint, dirty: true });
  assert.ok((await readFile(input.output, "utf8")).endsWith(signature));
  assert.deepEqual((await readdir(join(input.target_directory, "ctl-dev"))).sort(), ["cargo", "ctl"]);
  assert.equal(input.calls.some((call) => ["git", "xcrun", "spctl"].includes(call.command)), false);
  assert.equal(input.calls.some((call) => call.context.env.CTLD_SIGNING_TIMESTAMP === "secure"), false);
  assert.equal(input.calls.filter((call) => call.command === "cargo" && call.args[0] === "build").length, 2);
});

for (const failure of ["metadata", "helper-signing", "cli-signing", "cli-building", "architecture"] as const) {
  test(`failed ${failure} preserves the previous development CLI and cleans unpublished staging`, async (t) => {
    const input = await fixture(t, failure);
    await assert.rejects(buildSignedDevelopmentCli(input.options, input.run));
    assert.equal(await readFile(input.output, "utf8"), "previous usable ctl");
    assert.deepEqual((await readdir(join(input.target_directory, "ctl-dev"))).sort(), ["cargo", "ctl"]);
  });
}

test("unsupported native targets fail before building or signing", async (t) => {
  const input = await fixture(t);
  const run: MacosCommandRunner = (command, args, context) => command === "rustc"
    ? Promise.resolve({ stdout: "host: x86_64-unknown-linux-gnu\n", stderr: "" })
    : input.run(command, args, context);
  await assert.rejects(buildSignedDevelopmentCli(input.options, run), /native macOS/);
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
  const first = buildSignedDevelopmentCli(input.options, run);
  await first_started;
  const second = buildSignedDevelopmentCli(input.options, run);
  await second_metadata;
  assert.equal(helper_builds, 1);
  release_first();
  assert.deepEqual(await Promise.all([first, second]), [input.output, input.output]);
  assert.equal(helper_builds, 2);
  const packages = input.calls.filter((call) => call.command === "cargo" && call.args[0] === "build")
    .map((call) => call.args[call.args.indexOf("-p") + 1]);
  assert.deepEqual(packages, ["ctld", "ctl-cli", "ctld", "ctl-cli"]);
  assert.deepEqual((await readdir(join(input.target_directory, "ctl-dev"))).sort(), ["cargo", "ctl"]);
});
