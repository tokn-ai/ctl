import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import {
  binaryArtifact,
  buildCtlBundle,
  type BuildContext,
  type BuildCtlOptions,
  type BuildRunner,
} from "./build-ctl-bundle.mts";
import type { CtldBundleManifest } from "./package-ctld-bundle.mts";

const execute = promisify(execFile);
const team = "ABC123DEF4";
const fingerprint = "A".repeat(40);
const helperIdentifier = "dev.tokn-ai.ctl.ctld";
const cliIdentifier = "dev.tokn-ai.ctl.cli";
const signedMarker = Buffer.from("\nfixture Developer ID signature\n");

interface Fixture {
  root: string;
  options: BuildCtlOptions;
  helper_manifest: CtldBundleManifest;
  helper_archive: Buffer;
  cli_executable: string;
  ctld_executable: string;
}

interface Call {
  command: string;
  args: string[];
  context: BuildContext;
}

function profile(): Record<string, unknown> {
  return {
    Entitlements: {
      "com.apple.application-identifier": `${team}.${helperIdentifier}`,
      "com.apple.developer.team-identifier": team,
      "get-task-allow": false,
    },
    TeamIdentifier: [team],
    ProvisionsAllDevices: true,
    ExpirationDate: "2099-01-01T00:00:00Z",
  };
}

async function makeApp(app: string, version: string, binary: Buffer, stapled: boolean): Promise<void> {
  const contents = join(app, "Contents");
  await mkdir(join(contents, "MacOS"), { recursive: true });
  await mkdir(join(contents, "_CodeSignature"));
  await writeFile(join(contents, "MacOS", "ctld"), binary);
  await chmod(join(contents, "MacOS", "ctld"), 0o755);
  await writeFile(join(contents, "embedded.provisionprofile"), "fixture Developer ID profile");
  await writeFile(join(contents, "_CodeSignature", "CodeResources"), "fixture resource seal");
  await writeFile(join(contents, "Info.plist"), JSON.stringify({
    CFBundleIdentifier: helperIdentifier, CFBundleExecutable: "ctld",
    CFBundleShortVersionString: version, CFBundleVersion: version,
  }));
  if (stapled) await writeFile(join(contents, "CodeResources"), "fixture stapled ticket");
}

async function fixture(t: TestContext): Promise<Fixture> {
  const root = await mkdtemp(join(tmpdir(), "ctl-build-bundle-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const key = join(root, "notary.p8");
  const identity = join(root, "signing-identity");
  await writeFile(key, "fixture key; never submitted to Apple");
  await writeFile(identity, `${fingerprint}\n`);
  const options: BuildCtlOptions = {
    target: "aarch64-apple-darwin", app_version: "0.1.0",
    git_revision: "0123456789abcdef0123456789abcdef01234567",
    output_directory: join(root, "output"), ctld_assets: join(root, "helper-assets"),
    signing_identity_path: identity,
    env: {
      APPLE_API_KEY_PATH: key, APPLE_API_KEY: "TESTKEY123",
      APPLE_API_ISSUER: "11111111-2222-3333-4444-555555555555",
      CTL_BUNDLED_CTLD_DIR: "unrelated inherited payload",
      CTL_BUNDLED_CTLD_MODE: "development",
      CTLD_SIGNING_TIMESTAMP: "none",
    },
  };
  await mkdir(options.ctld_assets!);
  await makeApp(join(root, "ctld.app"), options.app_version, Buffer.from("fixture signed ctld"), true);
  const archive = `ctld-${options.app_version}-${options.target}.app.tar.gz`;
  const archivePath = join(options.ctld_assets!, archive);
  await execute("env", ["COPYFILE_DISABLE=1", "tar", "--format", "ustar", "-czf", archivePath, "-C", root, "ctld.app"]);
  const bytes = await readFile(archivePath);
  const helperManifest: CtldBundleManifest = {
    schema_version: 1, component: "ctld", app_version: options.app_version,
    bundle_id: options.app_version, git_revision: options.git_revision, target: options.target,
    bundle_identifier: helperIdentifier, team_identifier: team, signing_mode: "signed", notarized: true,
    archive, sha256: createHash("sha256").update(bytes).digest("hex"), archive_size: bytes.length,
  };
  await writeFile(join(options.ctld_assets!, `ctld-${options.target}.json`), JSON.stringify(helperManifest));
  const cliExecutable = join(root, "cargo-ctl");
  const ctldExecutable = join(root, "cargo-ctld");
  await writeFile(ctldExecutable, "fixture compiled ctld");
  return {
    root, options, helper_manifest: helperManifest, helper_archive: bytes,
    cli_executable: cliExecutable, ctld_executable: ctldExecutable,
  };
}

function artifact(name: string, executable: string): string {
  return JSON.stringify({
    reason: "compiler-artifact", target: { name, kind: ["bin"] }, profile: { test: false }, executable,
  });
}

function fakeBuild(input: Fixture, options: {
  dirty?: boolean;
  git_revision?: string;
  cargo_version?: string;
  notarization_status?: string;
  mutate_original_payload?: boolean;
  reject_cli_architecture?: boolean;
  reject_helper_signature?: boolean;
  reject_cli_notarization_check?: boolean;
} = {}): { run: BuildRunner; calls: Call[] } {
  const calls: Call[] = [];
  const provisioningProfile = profile();
  const run: BuildRunner = async (command, args, context) => {
    calls.push({ command, args, context });
    if (command === "git") {
      return {
        stdout: args[0] === "rev-parse"
          ? options.git_revision ?? input.options.git_revision
          : options.dirty ? " M Cargo.toml\n" : "",
        stderr: "",
      };
    }
    if (command === "cargo" && args[0] === "metadata") {
      assert.equal(context.env.CTL_BUNDLED_CTLD_DIR, undefined);
      assert.equal(context.env.CTL_BUNDLED_CTLD_MODE, undefined);
      return {
        stdout: JSON.stringify({ packages: ["ctld", "ctl-cli"].map((name) => ({
          name, version: options.cargo_version ?? input.options.app_version,
        })) }), stderr: "",
      };
    }
    if (command === "cargo" && args[0] === "build") {
      assert.ok(args.includes("--locked") && args.includes("--release"));
      assert.equal(args[args.indexOf("--target") + 1], input.options.target);
      if (args[args.indexOf("-p") + 1] === "ctld") {
        assert.equal(context.env.CTL_BUNDLED_CTLD_DIR, undefined);
        assert.equal(context.env.CTL_BUNDLED_CTLD_MODE, undefined);
        return { stdout: artifact("ctld", input.ctld_executable), stderr: "" };
      }
      const payload = context.env.CTL_BUNDLED_CTLD_DIR;
      assert.equal(context.env.CTL_BUNDLED_CTLD_MODE, "signed");
      assert.ok(payload);
      assert.notEqual(payload, input.options.ctld_assets);
      const manifest = JSON.parse(await readFile(join(payload, `ctld-${input.options.target}.json`), "utf8"));
      const bytes = await readFile(join(payload, manifest.archive));
      assert.equal(createHash("sha256").update(bytes).digest("hex"), manifest.sha256);
      const extracted = join(input.root, "embedded-helper");
      await mkdir(extracted);
      await execute("tar", ["-xzf", join(payload, manifest.archive), "-C", extracted]);
      assert.equal(await readFile(join(extracted, "ctld.app", "Contents", "CodeResources"), "utf8"), "fixture stapled ticket");
      if (options.mutate_original_payload) {
        await writeFile(join(input.options.ctld_assets!, input.helper_manifest.archive), "changed after snapshot");
      }
      await writeFile(input.cli_executable, Buffer.concat([Buffer.from("fixture compiled ctl\n"), bytes]));
      return { stdout: `compiler diagnostic\n${artifact("ctl", input.cli_executable)}\n`, stderr: "" };
    }
    if (command === "/bin/sh") {
      assert.equal(args[0], "scripts/ci/package-ctld-app.sh");
      assert.equal(context.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING, "true");
      assert.equal(context.env.CTLD_SIGNING_TIMESTAMP, "secure");
      await makeApp(args[2], args[3], await readFile(args[1]), false);
      await writeFile(context.env.CTLD_SIGNING_IDENTITY_OUTPUT!, fingerprint);
      return { stdout: "", stderr: "" };
    }
    if (command === "plutil") {
      if (args[0] === "-extract") {
        const value = provisioningProfile[args[1]];
        if (value === undefined) throw new Error("fixture profile field does not exist");
        if (args[2] === "xml1") {
          await writeFile(args[args.indexOf("-o") + 1], JSON.stringify(value));
          return { stdout: "", stderr: "" };
        }
        return { stdout: String(value), stderr: "" };
      }
      return { stdout: await readFile(args.at(-1)!, "utf8"), stderr: "" };
    }
    if (command === "security") return { stdout: "", stderr: "" };
    if (command === "codesign") {
      if (options.reject_cli_notarization_check && args.includes("--check-notarization")) {
        throw new Error("fixture rejected CLI notarization check");
      }
      if (options.reject_helper_signature && args.includes("--verify") && args.at(-1)?.endsWith("ctld.app")) {
        throw new Error("fixture rejected helper signature");
      }
      if (args.includes("--force")) {
        assert.ok(args.includes("runtime") && args.includes("--timestamp"));
        assert.equal(args[args.indexOf("--identifier") + 1], cliIdentifier);
        assert.equal(args[args.indexOf("--sign") + 1], fingerprint);
        await writeFile(args.at(-1)!, Buffer.concat([await readFile(args.at(-1)!), signedMarker]));
      } else if (args.includes("--entitlements")) {
        return { stdout: JSON.stringify(provisioningProfile.Entitlements), stderr: "" };
      } else if (args.includes("--verbose=4")) {
        return { stdout: "", stderr: "CodeDirectory v=20500 flags=0x10000(runtime)\nTimestamp=fixture timestamp\n" };
      }
      return { stdout: "", stderr: "" };
    }
    if (command === "lipo") {
      assert.equal(args[1], "arm64");
      if (options.reject_cli_architecture && args.at(-1)?.endsWith("/ctl")) {
        throw new Error("fixture rejected CLI architecture");
      }
      return { stdout: "", stderr: "" };
    }
    if (command === "ditto") {
      const source = args.at(-2)!;
      if ((await lstat(source)).isFile()) await copyFile(source, args.at(-1)!);
      else await writeFile(args.at(-1)!, "fixture helper submission ZIP");
      return { stdout: "", stderr: "" };
    }
    if (command === "xcrun" && args[0] === "notarytool") {
      assert.ok((await lstat(args[2])).isFile());
      return { stdout: JSON.stringify({ status: options.notarization_status ?? "Accepted" }), stderr: "" };
    }
    if (command === "xcrun" && args[0] === "stapler") {
      if (args[1] === "staple") await writeFile(join(args[2], "Contents", "CodeResources"), "fixture stapled ticket");
      assert.equal(await readFile(join(args[2], "Contents", "CodeResources"), "utf8"), "fixture stapled ticket");
      return { stdout: "", stderr: "" };
    }
    if (command === "spctl") return { stdout: "", stderr: "accepted\nsource=Notarized Developer ID\n" };
    if (command === "env" || command === "tar") return execute(command, args);
    throw new Error(`unexpected fixture command: ${command} ${args.join(" ")}`);
  };
  return { run, calls };
}

test("embeds a snapshot of the signed helper before signing, notarizing, and transporting the CLI", async (t) => {
  const input = await fixture(t);
  const build = fakeBuild(input, { mutate_original_payload: true });
  const manifest = await buildCtlBundle(input.options, build.run);
  assert.equal(manifest.bundled_ctld_sha256, input.helper_manifest.sha256);
  assert.equal(manifest.team_identifier, team);
  assert.equal(manifest.notarized, true);
  const archivePath = join(input.options.output_directory, manifest.archive);
  const bytes = await readFile(archivePath);
  assert.equal(manifest.sha256, createHash("sha256").update(bytes).digest("hex"));
  assert.equal(manifest.archive_size, bytes.length);
  assert.equal(await readFile(`${archivePath}.sha256`, "utf8"), `${manifest.sha256}  ${manifest.archive}\n`);
  assert.deepEqual(JSON.parse(await readFile(join(input.options.output_directory, `ctl-cli-${input.options.target}.json`), "utf8")), manifest);
  const extracted = join(input.root, "transported-cli");
  await mkdir(extracted);
  await execute("tar", ["-xzf", archivePath, "-C", extracted]);
  assert.deepEqual(await readdir(extracted), ["ctl"]);
  const cli = await readFile(join(extracted, "ctl"));
  assert.ok(cli.includes(input.helper_archive));
  assert.ok(cli.includes(signedMarker));
  assert.equal((await lstat(join(extracted, "ctl"))).mode & 0o777, 0o755);
  const compile = build.calls.findIndex(({ command, args }) => command === "cargo" && args[0] === "build" && args.includes("ctl-cli"));
  const helperCheck = build.calls.findIndex(({ command, args }) => command === "codesign" && args.includes("--verify") && args.at(-1)?.endsWith("ctld.app"));
  const sign = build.calls.findIndex(({ command, args }) => command === "codesign" && args.includes("--force"));
  const notary = build.calls.findIndex(({ command, args }) => command === "xcrun" && args[0] === "notarytool");
  const archive = build.calls.findIndex(({ command }) => command === "env");
  let transportedCheck = -1;
  for (const [index, { command, args }] of build.calls.entries()) {
    if (command === "codesign" && args.at(-1)?.includes("transported") && !args.includes("--check-notarization")) transportedCheck = index;
  }
  assert.ok(helperCheck >= 0 && helperCheck < compile && compile < sign && sign < notary && notary < archive && archive < transportedCheck);
  const requirement = build.calls[transportedCheck].args[build.calls[transportedCheck].args.indexOf("--test-requirement") + 1];
  assert.ok(requirement.includes(cliIdentifier) && requirement.includes(team));
  assert.ok(requirement.includes("1.2.840.113635.100.6.1.13"));
  const notarizationChecks = build.calls
    .map((call, index) => ({ ...call, index }))
    .filter(({ command, args }) => command === "codesign" && args.includes("--check-notarization"));
  assert.equal(notarizationChecks.length, 2);
  assert.ok(notary < notarizationChecks[0].index && notarizationChecks[0].index < archive);
  assert.ok(transportedCheck < notarizationChecks[1].index);
  for (const { args } of notarizationChecks) {
    assert.equal(args[args.indexOf("--test-requirement") + 1], "=notarized");
  }
  assert.ok(!build.calls.some(({ command, args }) => command === "spctl" && args.at(-1)?.endsWith("/ctl")));
  const payload = build.calls.find(({ command, args }) => command === "cargo" && args[0] === "build")!.context.env.CTL_BUNDLED_CTLD_DIR!;
  await assert.rejects(lstat(payload), /ENOENT/);
});

test("builds, signs, notarizes, and staples ctld before compiling its bundled CLI", async (t) => {
  const input = await fixture(t);
  const options = { ...input.options, ctld_assets: undefined, signing_identity_path: undefined };
  const build = fakeBuild(input);
  const manifest = await buildCtlBundle(options, build.run);
  assert.equal(manifest.team_identifier, team);
  const helperCompile = build.calls.findIndex(({ command, args }) => command === "cargo" && args.includes("ctld"));
  const helperPackage = build.calls.findIndex(({ command }) => command === "/bin/sh");
  const helperNotary = build.calls.findIndex(({ command, args }) => command === "xcrun" && args[0] === "notarytool");
  const helperStaple = build.calls.findIndex(({ command, args }) => command === "xcrun" && args[1] === "staple");
  const cliCompile = build.calls.findIndex(({ command, args }) => command === "cargo" && args.includes("ctl-cli"));
  assert.ok(helperCompile >= 0 && helperCompile < helperPackage && helperPackage < helperNotary && helperNotary < helperStaple && helperStaple < cliCompile);
  assert.equal(build.calls.filter(({ command, args }) => command === "xcrun" && args[0] === "notarytool").length, 2);
});

test("rejects dirty checkouts, wrong revisions, and incompatible Cargo versions before compiling", async (t) => {
  for (const changes of [{ dirty: true }, { git_revision: "f".repeat(40) }, { cargo_version: "0.2.0" }]) {
    await t.test(JSON.stringify(changes), async (t) => {
      const input = await fixture(t);
      const build = fakeBuild(input, changes);
      await assert.rejects(buildCtlBundle(input.options, build.run), /clean checkout|Cargo version/);
      assert.ok(!build.calls.some(({ command, args }) => command === "cargo" && args[0] === "build"));
      assert.ok(!build.calls.some(({ command }) => command === "codesign" || command === "xcrun"));
    });
  }
});

test("rejects unsupported targets and malformed release versions before invoking tools", async (t) => {
  const input = await fixture(t);
  for (const changes of [{ target: "aarch64-unknown-linux-gnu" }, { app_version: "../0.1.0" }]) {
    const build = fakeBuild(input);
    await assert.rejects(buildCtlBundle({ ...input.options, ...changes }, build.run), /macOS target, release version/);
    assert.deepEqual(build.calls, []);
  }
});

test("rejects damaged or mismatched helper inputs before compiling the CLI", async (t) => {
  for (const changes of [
    { sha256: "0".repeat(64) }, { archive_size: 1 }, { target: "x86_64-apple-darwin" },
    { app_version: "0.2.0" }, { signing_mode: "unsigned" }, { notarized: false },
    { team_identifier: 1234567890 },
  ]) {
    await t.test(JSON.stringify(changes), async (t) => {
      const input = await fixture(t);
      await writeFile(join(input.options.ctld_assets!, `ctld-${input.options.target}.json`), JSON.stringify({ ...input.helper_manifest, ...changes }));
      const build = fakeBuild(input);
      await assert.rejects(buildCtlBundle(input.options, build.run), /payload|ctld manifest/);
      assert.ok(!build.calls.some(({ command, args }) => command === "cargo" && args[0] === "build"));
      assert.deepEqual(await readdir(input.options.output_directory), []);
    });
  }
});

test("does not emit CLI archives after rejected notarization or architecture checks", async (t) => {
  for (const changes of [{ notarization_status: "Invalid" }, { reject_cli_architecture: true }, { reject_cli_notarization_check: true }]) {
    await t.test(JSON.stringify(changes), async (t) => {
      const input = await fixture(t);
      const build = fakeBuild(input, changes);
      await assert.rejects(buildCtlBundle(input.options, build.run), /notarization was not accepted|rejected CLI architecture|rejected CLI notarization check/);
      assert.deepEqual(await readdir(input.options.output_directory), []);
      assert.ok(!build.calls.some(({ command }) => command === "env"));
    });
  }
});

test("rejects a reused helper signature before Cargo can embed it", async (t) => {
  const input = await fixture(t);
  const build = fakeBuild(input, { reject_helper_signature: true });
  await assert.rejects(buildCtlBundle(input.options, build.run), /rejected helper signature/);
  assert.ok(!build.calls.some(({ command, args }) => command === "cargo" && args[0] === "build"));
  assert.deepEqual(await readdir(input.options.output_directory), []);
});

test("rejects helper archive links before extraction or signature assessment", async (t) => {
  const input = await fixture(t);
  await symlink("MacOS/ctld", join(input.root, "ctld.app", "Contents", "linked-helper"));
  const archivePath = join(input.options.ctld_assets!, input.helper_manifest.archive);
  await execute("env", ["COPYFILE_DISABLE=1", "tar", "--format", "ustar", "-czf", archivePath, "-C", input.root, "ctld.app"]);
  const bytes = await readFile(archivePath);
  await writeFile(join(input.options.ctld_assets!, `ctld-${input.options.target}.json`), JSON.stringify({
    ...input.helper_manifest, sha256: createHash("sha256").update(bytes).digest("hex"), archive_size: bytes.length,
  }));
  const build = fakeBuild(input);
  await assert.rejects(buildCtlBundle(input.options, build.run), /unsupported ctld archive entry/);
  assert.ok(!build.calls.some(({ command, args }) => command === "tar" && args[0] === "-xzf"));
  assert.ok(!build.calls.some(({ command, args }) => command === "codesign" || command === "cargo" && args[0] === "build"));
  assert.deepEqual(await readdir(input.options.output_directory), []);
});

test("Cargo executable selection rejects missing and ambiguous non-test binary artifacts", () => {
  assert.throws(() => binaryArtifact("diagnostic\n", "ctl"), /one executable/);
  assert.throws(() => binaryArtifact(`${artifact("ctl", "/tmp/first")}\n${artifact("ctl", "/tmp/second")}`, "ctl"), /one executable/);
  const ignored = JSON.stringify({ reason: "compiler-artifact", target: { name: "ctl", kind: ["bin"] }, profile: { test: true }, executable: "/tmp/test" });
  assert.equal(binaryArtifact(`${ignored}\n${artifact("ctl", "/tmp/actual")}`, "ctl"), "/tmp/actual");
});
