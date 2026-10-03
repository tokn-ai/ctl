import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, rename, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import {
  packageCtldBundle,
  validateDistributionProfile,
  type CtldBundleOptions,
  type ProcessRunner,
} from "./package-ctld-bundle.mts";

const execute = promisify(execFile);
const team = "ABC123DEF4";
const protocols = [{name:"ctld",build:12,version:"1.0.12",supported_versions:["1.0.12"]}, ...["ctld_lifecycle", "ctld_helper"].map((name) => ({name,build:1,version:"1.0.1",supported_versions:["1.0.1"]}))];
const identifier = "dev.tokn-ai.ctl.ctld";

function profile(): Record<string, unknown> {
  return {
    Entitlements: {
      "com.apple.application-identifier": `${team}.${identifier}`,
      "com.apple.developer.team-identifier": team,
      "get-task-allow": false,
    },
    TeamIdentifier: [team],
    ProvisionsAllDevices: true,
    ExpirationDate: "2099-01-01T00:00:00Z",
  };
}

async function fixture(context: TestContext): Promise<CtldBundleOptions> {
  const root = await mkdtemp(join(tmpdir(), "ctld-bundle-test-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const app = join(root, "ctld.app");
  await mkdir(join(app, "Contents", "MacOS"), { recursive: true });
  await mkdir(join(app, "Contents", "_CodeSignature"));
  await writeFile(join(app, "Contents", "MacOS", "ctld"), "fixture Mach-O executable");
  await chmod(join(app, "Contents", "MacOS", "ctld"), 0o755);
  await writeFile(join(app, "Contents", "embedded.provisionprofile"), "fixture distribution profile");
  await writeFile(join(app, "Contents", "_CodeSignature", "CodeResources"), "signature resource seal");
  await writeFile(join(app, "Contents", "Info.plist"), JSON.stringify({
    CFBundleIdentifier: identifier, CFBundleExecutable: "ctld", CFBundleShortVersionString: "0.1.0", CFBundleVersion: "0.1.0",
  }));
  const key = join(root, "test-notary-key.p8");
  await writeFile(key, "test key; never submitted to Apple");
  return {
    target: "aarch64-apple-darwin",
    app_version: "0.1.0",
    bundle_id: "0.1.0",
    git_revision: "0123456789abcdef0123456789abcdef01234567",
    input_app: app,
    output_directory: join(root, "assets"),
    notary_key_path: key,
    notary_key_id: "TESTKEY123",
    notary_issuer: "11111111-2222-3333-4444-555555555555",
  };
}

function fakeApple(options: {
  provisioning_profile?: Record<string, unknown>;
  signed_entitlements?: unknown;
  notarization_status?: string;
  signature_details?: string;
  fail_command?: string;
  component_metadata?: unknown;
} = {}): { run: ProcessRunner; calls: { command: string; args: string[] }[] } {
  const calls: { command: string; args: string[] }[] = [];
  const provisioningProfile = options.provisioning_profile ?? profile();
  const run: ProcessRunner = async (command, args) => {
    calls.push({ command, args });
    if (args[0] === "--component-info") {
      return { stdout: JSON.stringify(options.component_metadata ?? { build: {
        version: "0.1.0", source_revision: "0123456789abcdef0123456789abcdef01234567", source_fingerprint: "a".repeat(64), dirty: false,
      }, protocols }), stderr: "" };
    }
    if (command === "lipo") {
      assert.equal(args.length, 3);
      assert.ok(args[0].endsWith("/Contents/MacOS/ctld"));
      assert.equal(args[1], "-verify_arch");
      assert.ok(["arm64", "x86_64"].includes(args[2]));
    }
    if (command === options.fail_command) {
      throw new Error(`fixture rejected ${command}`);
    }
    if (command === "plutil") {
      if (args[0] === "-extract") {
        const value = provisioningProfile[args[1]];
        if (value === undefined) {
          throw new Error("fixture field does not exist");
        }
        if (args[2] === "json") {
          throw new Error("full provisioning profiles contain Date/Data that cannot be converted to JSON");
        }
        if (args[2] === "xml1") {
          await writeFile(args[args.indexOf("-o") + 1], JSON.stringify(value));
          return { stdout: "", stderr: "" };
        }
        return { stdout: args[2] === "raw" ? String(value) : JSON.stringify(value), stderr: "" };
      }
      const path = args.at(-1)!;
      return {
        stdout: path.endsWith("entitlements.plist")
          ? JSON.stringify(options.signed_entitlements ?? profile().Entitlements)
          : await readFile(path, "utf8"),
        stderr: "",
      };
    }
    if (command === "codesign" && args.includes("--verbose=4")) {
      return { stdout: "", stderr: options.signature_details ?? "CodeDirectory v=20500 size=123 flags=0x10000(runtime)\nTimestamp=Oct 2, 2026 at 1:00 AM\n" };
    }
    if (command === "xcrun" && args[0] === "notarytool") {
      return { stdout: JSON.stringify({ status: options.notarization_status ?? "Accepted" }), stderr: "" };
    }
    if (command === "xcrun" && args[0] === "stapler" && args[1] === "staple") {
      await writeFile(join(args[2], "Contents", "CodeResources"), "stapled notarization ticket");
    }
    if (command === "env" || command === "tar") {
      return execute(command, args);
    }
    return { stdout: "", stderr: "" };
  };
  return { run, calls };
}

test("notarizes before archiving and validates the transported full bundle", async (context) => {
  const options = await fixture(context);
  const apple = fakeApple();
  const manifest = await packageCtldBundle(options, apple.run);
  assert.deepEqual(manifest, {
    schema_version: 1, component: "ctld", app_version: "0.1.0", bundle_id: "0.1.0",
    git_revision: options.git_revision, target: options.target, bundle_identifier: identifier,
    team_identifier: team, signing_mode: "signed", notarized: true,
    archive: "ctld-0.1.0-aarch64-apple-darwin.app.tar.gz",
    sha256: manifest.sha256, archive_size: manifest.archive_size, protocols,
  });
  const archive = join(options.output_directory, manifest.archive);
  const bytes = await readFile(archive);
  assert.equal(manifest.sha256, createHash("sha256").update(bytes).digest("hex"));
  assert.equal(manifest.archive_size, bytes.length);
  const extracted = join(dirname(options.input_app), "test-extracted");
  await mkdir(extracted);
  await execute("tar", ["-xzf", archive, "-C", extracted]);
  assert.deepEqual(await readdir(extracted), ["ctld.app"]);
  const contents = join(extracted, "ctld.app", "Contents");
  assert.equal(await readFile(join(contents, "CodeResources"), "utf8"), "stapled notarization ticket");
  assert.equal(await readFile(join(contents, "_CodeSignature", "CodeResources"), "utf8"), "signature resource seal");
  assert.equal(await readFile(join(contents, "embedded.provisionprofile"), "utf8"), "fixture distribution profile");
  assert.equal((await lstat(join(contents, "MacOS", "ctld"))).mode & 0o777, 0o755);
  assert.deepEqual(JSON.parse(await readFile(join(options.output_directory, `ctld-${options.target}.json`), "utf8")), manifest);
  assert.equal(await readFile(`${archive}.sha256`, "utf8"), `${manifest.sha256}  ${manifest.archive}\n`);
  const submission = apple.calls.findIndex(({ command, args }) => command === "xcrun" && args[0] === "notarytool");
  const staple = apple.calls.findIndex(({ command, args }) => command === "xcrun" && args[1] === "staple");
  const tar = apple.calls.findIndex(({ command }) => command === "env");
  assert.ok(submission < staple && staple < tar);
  assert.ok(apple.calls[tar].args.includes("COPYFILE_DISABLE=1"));
  assert.equal(apple.calls.filter(({ command, args }) => command === "xcrun" && args[1] === "validate").length, 2);
  assert.equal(apple.calls.filter(({ command }) => command === "spctl").length, 2);
  for (const { command, args } of apple.calls) {
    if (command === "plutil" && args[0] === "-extract") {
      assert.notEqual(args[2], "json");
    }
  }
  const requirement = apple.calls.find(({ command, args }) => command === "codesign" && args.includes("--test-requirement"))!.args.join(" ");
  assert.ok(requirement.includes(identifier) && requirement.includes(team));
  assert.ok(requirement.includes("1.2.840.113635.100.6.1.13"));
  for (const { command, args } of apple.calls) {
    if (command === "codesign" && args.includes("--test-requirement")) {
      assert.ok(args[args.indexOf("--test-requirement") + 1].startsWith("=anchor "));
    }
  }
});

test("native plutil handles provisioning dates and certificate data before fake signing", { skip: process.platform !== "darwin" }, async (context) => {
  const options = await fixture(context);
  const apple = fakeApple();
  let decodedProfile = "";
  const run: ProcessRunner = async (command, args) => {
    if (command === "security" && args[0] === "cms") {
      decodedProfile = args.at(-1)!;
      await writeFile(decodedProfile, `<?xml version="1.0" encoding="UTF-8"?>
        <plist version="1.0"><dict>
        <key>Entitlements</key><dict>
          <key>com.apple.application-identifier</key><string>${team}.${identifier}</string>
          <key>com.apple.developer.team-identifier</key><string>${team}</string>
          <key>get-task-allow</key><false/>
        </dict>
        <key>TeamIdentifier</key><array><string>${team}</string></array>
        <key>ProvisionsAllDevices</key><true/>
        <key>ExpirationDate</key><date>2099-01-01T00:00:00Z</date>
        <key>DeveloperCertificates</key><array><data>AQI=</data></array>
        </dict></plist>`);
      return { stdout: "", stderr: "" };
    }
    if (command === "plutil") return execute("/usr/bin/plutil", args);
    if (command === "codesign" && args.includes("--entitlements")) {
      return execute("/usr/bin/plutil", ["-extract", "Entitlements", "xml1", "-o", "-", decodedProfile]);
    }
    return apple.run(command, args);
  };
  const manifest = await packageCtldBundle(options, run);
  assert.equal(manifest.team_identifier, team);
});

test("native lipo verifies the helper's architecture through the packaging pipeline", { skip: process.platform !== "darwin" }, async (context) => {
  const options = await fixture(context);
  const architecture = process.arch === "arm64" ? "arm64" : "x86_64";
  options.target = architecture === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
  const binary = join(options.input_app, "Contents/MacOS/ctld");
  await copyFile(process.execPath, binary);
  const apple = fakeApple();
  const run: ProcessRunner = async (command, args) => {
    const result = await apple.run(command, args);
    return command === "lipo" ? execute("/usr/bin/lipo", args) : result;
  };
  const manifest = await packageCtldBundle(options, run);
  assert.equal(manifest.target, options.target);
  assert.deepEqual(apple.calls.find(({ command }) => command === "lipo")?.args, [
    binary, "-verify_arch", architecture,
  ]);
  assert.ok((await lstat(join(options.output_directory, manifest.archive))).isFile());
});

test("native lipo rejects an absent target architecture before signing or producing assets", { skip: process.platform !== "darwin" }, async (context) => {
  const options = await fixture(context);
  const architecture = process.arch === "arm64" ? "arm64" : "x86_64";
  const missing = architecture === "arm64" ? "x86_64" : "arm64";
  options.target = missing === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
  const binary = join(options.input_app, "Contents/MacOS/ctld");
  await copyFile(process.execPath, binary);
  // A universal Node distribution may contain both supported release targets.
  // Reduce it to the current architecture so the other target is always absent.
  const { stdout } = await execute("/usr/bin/lipo", [binary, "-archs"]);
  if (stdout.trim().split(/\s+/).length > 1) {
    const thin = join(dirname(options.input_app), "thin-ctld");
    await execute("/usr/bin/lipo", [binary, "-thin", architecture, "-output", thin]);
    await rename(thin, binary);
  }
  const before = createHash("sha256").update(await readFile(binary)).digest("hex");
  const apple = fakeApple();
  const run: ProcessRunner = async (command, args) => {
    const result = await apple.run(command, args);
    return command === "lipo" ? execute("/usr/bin/lipo", args) : result;
  };
  await assert.rejects(packageCtldBundle(options, run), (error: unknown) => {
    const failure = error as { code?: number; stderr?: string };
    assert.equal(failure.code, 1);
    // A malformed lipo argument list must not count as architecture rejection.
    assert.ok(!failure.stderr?.includes("unknown architecture specification"));
    return true;
  });
  assert.deepEqual(apple.calls.find(({ command }) => command === "lipo")?.args, [
    binary, "-verify_arch", missing,
  ]);
  assert.ok(!apple.calls.some(({ command }) => command === "codesign" || command === "xcrun"));
  assert.deepEqual(await readdir(options.output_directory), []);
  assert.equal(createHash("sha256").update(await readFile(binary)).digest("hex"), before);
});

test("rejects development, expired, and mismatched provisioning profiles", async (context) => {
  const invalidProfiles = [
    { ...profile(), ProvisionsAllDevices: false },
    { ...profile(), ProvisionedDevices: ["test device"] },
    { ...profile(), ExpirationDate: "2020-01-01T00:00:00Z" },
    { ...profile(), TeamIdentifier: ["OTHERTEAM1"] },
    { ...profile(), Entitlements: { ...(profile().Entitlements as object), "get-task-allow": true } },
    { ...profile(), Entitlements: { ...(profile().Entitlements as object), "com.apple.security.get-task-allow": true } },
    { ...profile(), Entitlements: { ...(profile().Entitlements as object), "com.apple.application-identifier": `${team}.wrong` } },
  ];
  for (const provisioning_profile of invalidProfiles) {
    await context.test(JSON.stringify(provisioning_profile), async (context) => {
      const options = await fixture(context);
      const apple = fakeApple({ provisioning_profile });
      await assert.rejects(packageCtldBundle(options, apple.run), /profile/);
      assert.ok(!apple.calls.some(({ command }) => command === "xcrun"));
      assert.deepEqual(await readdir(options.output_directory), []);
    });
  }
  assert.equal(validateDistributionProfile(profile()), team);
});

test("does not produce release assets after signing or notarization failure", async (context) => {
  for (const appleOptions of [
    { fail_command: "codesign" },
    { fail_command: "lipo" },
    { fail_command: "spctl" },
    { notarization_status: "Invalid" },
    { signature_details: "CodeDirectory flags=0\nTimestamp=now\n" },
    { signature_details: "CodeDirectory flags=0x10000(runtime)\n" },
    { signed_entitlements: { ...(profile().Entitlements as object), "get-task-allow": true } },
    { signed_entitlements: { ...(profile().Entitlements as object), "com.apple.security.get-task-allow": true } },
  ]) {
    await context.test(JSON.stringify(appleOptions), async (context) => {
      const options = await fixture(context);
      const apple = fakeApple(appleOptions);
      await assert.rejects(packageCtldBundle(options, apple.run));
      assert.deepEqual(await readdir(options.output_directory), []);
    });
  }
});

test("rejects helper links and stale output", async (context) => {
  await context.test("symlink", async (context) => {
    const options = await fixture(context);
    await symlink("MacOS/ctld", join(options.input_app, "Contents", "linked-ctld"));
    await assert.rejects(packageCtldBundle(options, fakeApple().run), /links or special files/);
  });
  await context.test("stale assets", async (context) => {
    const options = await fixture(context);
    await mkdir(options.output_directory);
    await writeFile(join(options.output_directory, "old.json"), "old");
    const apple = fakeApple();
    await assert.rejects(packageCtldBundle(options, apple.run), /must be empty/);
    assert.deepEqual(apple.calls, []);
  });
});

test("rejects unsupported targets, invalid identities, and absent notarization credentials before submission", async (context) => {
  const options = await fixture(context);
  for (const changes of [
    { target: "x86_64-unknown-linux-gnu" },
    { app_version: "unknown" },
    { bundle_id: "../outside" },
    { git_revision: "f".repeat(39) },
    { notary_key_path: "" },
  ]) {
    const apple = fakeApple();
    await assert.rejects(packageCtldBundle({ ...options, ...changes }, apple.run));
    assert.deepEqual(apple.calls, []);
  }
});


test("component source and protocol advertisements are verified before notarization", async (context) => {
  for (const changed of [
    {build:{version:"0.1.0",source_revision:"f".repeat(40),source_fingerprint:"a".repeat(64),dirty:false},protocols},
    {build:{version:"0.1.0",source_revision:"0123456789abcdef0123456789abcdef01234567",source_fingerprint:"a".repeat(64),dirty:true},protocols},
    {build:{version:"0.1.0",source_revision:"0123456789abcdef0123456789abcdef01234567",source_fingerprint:"a".repeat(64),dirty:false},protocols:[]},
  ]) {
    const options = await fixture(context);
    const apple = fakeApple({component_metadata:changed});
    await assert.rejects(packageCtldBundle(options, apple.run), /source identity|protocol/);
    assert.equal(apple.calls.some(({command,args}) => command === "xcrun" && args[0] === "notarytool"), false);
    assert.deepEqual(await readdir(options.output_directory), []);
  }
});
