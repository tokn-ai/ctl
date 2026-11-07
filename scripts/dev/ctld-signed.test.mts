import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { signDevelopmentHelper, type DevelopmentSigningOptions } from "./ctld-signed.mts";
import type { MacosCommandContext, MacosCommandRunner } from "./macos-provisioning.mts";

const execute = promisify(execFile);
const protocols = [{ name: "ctld", build: 12, version: "1.0.12", supported_versions: ["1.0.12"] },
  ...["ctld_lifecycle", "ctld_helper"].map((name) => ({ name, build: 1, version: "1.0.1", supported_versions: ["1.0.1"] }))];

async function fixture(t: TestContext, settings: { failure?: "metadata" | "signing" | "architecture" | "signed-metadata"; mutate_original?: boolean; native_binary?: boolean } = {}) {
  const root = await mkdtemp(join(tmpdir(), "ctld-prebuilt-signing-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const output = join(root, "output"), target_directory = join(root, "custom Cargo cache");
  const artifact = join(root, "prebuilt-ctld"), profile = join(root, "profile");
  const bytes = settings.native_binary ? await readFile(process.execPath) : Buffer.from("prebuilt immutable ctld");
  await writeFile(artifact, bytes);
  await writeFile(profile, "fixture provisioned profile");
  await mkdir(target_directory);
  const target = settings.native_binary && process.arch === "x64" ? "x86_64-apple-darwin" : "aarch64-apple-darwin";
  let probes = 0;
  const calls: { command: string; args: string[]; context: MacosCommandContext }[] = [];
  const run: MacosCommandRunner = async (command, args, context) => {
    calls.push({ command, args, context });
    const empty = { stdout: "", stderr: "" };
    if (command === "security") return empty;
    if (command === "/usr/libexec/PlistBuddy") return { stdout: args[1]?.endsWith(":ExpirationDate") ? "2099-01-01T00:00:00Z" : "TEAM123ABC.dev.tokn-ai.ctl.ctld", stderr: "" };
    if (args[0] === "--component-info") {
      assert.notEqual(command, artifact, "signing probes its own immutable copy");
      assert.deepEqual(await readFile(command), bytes);
      probes += 1;
      if (settings.mutate_original) await writeFile(artifact, "concurrent Cargo replacement");
      return { stdout: JSON.stringify({ build: { version: "0.1.0", source_revision: "b".repeat(40),
        source_fingerprint: settings.failure === "metadata" ? "malformed" :
          settings.failure === "signed-metadata" && probes > 1 ? "d".repeat(64) : "c".repeat(64), dirty: true }, protocols }), stderr: "" };
    }
    if (command === "/bin/sh") {
      if (settings.failure === "signing") throw new Error("fixture helper signing failed");
      assert.equal(context.env.CTLD_SIGNING_TIMESTAMP, "none");
      assert.equal(context.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING, "false");
      assert.equal(context.env.CTLD_PROVISIONING_PROFILE, profile);
      assert.equal(context.env.CTL_BUNDLED_CTLD_DIR, undefined);
      const app = args[2]!;
      await mkdir(join(app, "Contents/MacOS"), { recursive: true });
      await mkdir(join(app, "Contents/_CodeSignature"));
      await copyFile(args[1]!, join(app, "Contents/MacOS/ctld"));
      await chmod(join(app, "Contents/MacOS/ctld"), 0o755);
      await copyFile(profile, join(app, "Contents/embedded.provisionprofile"));
      await writeFile(join(app, "Contents/Info.plist"), "fixture metadata");
      await writeFile(join(app, "Contents/_CodeSignature/CodeResources"), "fixture signature");
      await writeFile(context.env.CTLD_SIGNING_IDENTITY_OUTPUT!, "A".repeat(40));
      return empty;
    }
    if (command === "lipo") {
      assert.equal(args[1], "-verify_arch");
      assert.equal(args[2], target.startsWith("aarch64") ? "arm64" : "x86_64");
      if (settings.failure === "architecture") throw new Error("fixture architecture mismatch");
      return settings.native_binary ? execute("/usr/bin/lipo", args) : empty;
    }
    if (command === "codesign") return args.includes("-d") ? { stdout: "", stderr: "TeamIdentifier=TEAM123ABC\n" } : empty;
    if (command === "env") return execute(command, args);
    throw new Error(`Unexpected signing operation: ${command}`);
  };
  const options: DevelopmentSigningOptions = { repository_root: root, target_directory, artifact, target, app_version: "0.1.0", output_directory: output,
    home_directory: join(root, "home"), env: { CTLD_PROVISIONING_PROFILE: profile, CTLD_SIGNING_TIMESTAMP: "secure",
      CTLD_REQUIRE_DISTRIBUTION_SIGNING: "true", CTL_BUNDLED_CTLD_DIR: "unrelated release payload", CTL_BUNDLED_CTLD_MODE: "release" } };
  return { root, output, target_directory, artifact, bytes, options, run, calls };
}

test("signing a prebuilt helper preserves source identity and emits a complete receipt without compiling or selecting anything", async (t) => {
  const input = await fixture(t, { mutate_original: true });
  const result = await signDevelopmentHelper(input.options, input.run);
  assert.equal(result.manifest.signing_mode, "development");
  assert.equal(result.manifest.notarized, false);
  assert.equal(result.manifest.bundle_id, `dev.${result.manifest.sha256}`);
  assert.equal(result.manifest.git_revision, "b".repeat(40));
  assert.deepEqual(result.manifest.development, { source_fingerprint: "c".repeat(64), dirty: true });
  assert.deepEqual(result.manifest.protocols, protocols);
  assert.deepEqual(JSON.parse(await readFile(join(input.output, "installation.json"), "utf8")), result.manifest);
  assert.deepEqual(await readFile(join(result.app, "Contents/MacOS/ctld")), input.bytes);
  assert.equal(await readFile(input.artifact, "utf8"), "concurrent Cargo replacement");
  assert.deepEqual((await readdir(input.output)).sort(), ["ctld.app", "installation.json"]);
  assert.deepEqual(await readdir(input.target_directory), []);
  assert.equal(input.calls.some((call) => ["cargo", "git", "xcodebuild", "spctl"].includes(call.command)), false);
});

for (const failure of ["metadata", "signing", "architecture", "signed-metadata"] as const) {
  test(`failed ${failure} cannot publish a successful receipt or modify the prebuilt input`, async (t) => {
    const input = await fixture(t, { failure });
    await assert.rejects(signDevelopmentHelper(input.options, input.run));
    assert.deepEqual(await readFile(input.artifact), input.bytes);
    await assert.rejects(readFile(join(input.output, "installation.json")), { code: "ENOENT" });
    assert.ok((await readdir(input.output)).every((name) => !name.startsWith('.')));
    assert.deepEqual(await readdir(input.target_directory), []);
  });
}

test("a foreign signing target fails before touching the recipe or output", async (t) => {
  const input = await fixture(t);
  await assert.rejects(signDevelopmentHelper({ ...input.options, target: "x86_64-unknown-linux-gnu" }, input.run), /native macOS/);
  assert.deepEqual(input.calls, []);
  await assert.rejects(lstat(input.output), { code: "ENOENT" });
});

test("the native Mach-O parser verifies the signed copy's architecture", { skip: process.platform !== "darwin" }, async (t) => {
  const input = await fixture(t, { native_binary: true });
  const result = await signDevelopmentHelper(input.options, input.run);
  assert.deepEqual(await readFile(join(result.app, "Contents/MacOS/ctld")), input.bytes);
  assert.equal(input.calls.filter((call) => call.command === "lipo").length, 1);
});
