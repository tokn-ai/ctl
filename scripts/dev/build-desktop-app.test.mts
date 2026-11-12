import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, cp, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { componentMapFixture } from "../ci/agent-bundle.fixtures.mts";
import { extractWindowsPortableFixture, windowsExecutableFixture } from "../shared/windows-portable.fixtures.mts";
import { buildDesktopApp, runDesktopBuildCommand, type DesktopBuildOptions } from "./build-desktop-app.mts";
import type { buildDaemons } from "./daemon-build.mts";
import type { MacosCommandRunner } from "./macos-provisioning.mts";

const execute = promisify(execFile);

async function fixture(t: TestContext, settings: {
  target?: string; profile?: boolean; output?: "missing" | "failed"; mixed_version?: boolean;
  signing_failure?: boolean; invalid_helper?: boolean; configured_project?: boolean;
  profile_expired?: boolean; renewal_failure?: boolean; profile_from_home?: boolean;
} = {}) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "ctl-desktop-source-test-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  const workspace = join(root, "workspace"), output = join(root, "output"), home = join(root, "home");
  const target_directory = join(root, "custom Cargo cache"), native = settings.target ?? "x86_64-unknown-linux-gnu";
  const config_directory = join(workspace, "apps/desktop/src-tauri");
  for (const directory of [workspace, output, home]) await mkdir(directory, { mode: 0o700 });
  await mkdir(config_directory, { recursive: true });
  await mkdir(join(config_directory, "resources/agent-bundles"), { recursive: true });
  await writeFile(join(config_directory, "tauri.conf.json"), JSON.stringify({
    productName: "ctmux", version: settings.mixed_version ? "0.1.1" : "0.1.0",
  }));
  const profile = settings.profile_from_home
    ? join(home, "Library/Developer/Xcode/UserData/Provisioning Profiles/fixture.provisionprofile")
    : join(root, "profile");
  let profile_available = settings.profile ?? false;
  let profile_expired = settings.profile_expired ?? false;
  if (profile_available) {
    await mkdir(dirname(profile), { recursive: true });
    await writeFile(profile, "fixture provisioning profile");
  }
  const provisioning_project = join(target_directory, "ctld-provisioning/ctld-provisioning.xcodeproj");
  if (settings.configured_project) {
    await mkdir(provisioning_project, { recursive: true });
    await writeFile(join(provisioning_project, "project.pbxproj"), "DEVELOPMENT_TEAM = TEAM123ABC;");
  }
  const checkout = createHash("sha256").update(workspace).digest("hex").slice(0, 20);
  const cache = join(target_directory, "ctl-desktop-build", checkout, "cargo");
  const macos = native.endsWith("-apple-darwin");
  const windows = native.endsWith("-pc-windows-msvc");
  const windows_app = join(cache, native, "debug", "ctmux-app.exe");
  const bundle_directory = join(cache, native, "debug/bundle", macos ? "macos" : "appimage");
  await mkdir(bundle_directory, { recursive: true, mode: 0o700 });
  await writeFile(join(bundle_directory, "stale-app"), "earlier build");
  if (windows) await writeFile(windows_app, windowsExecutableFixture(native));
  await writeFile(join(config_directory, "resources/agent-bundles/.resource"), "bundled resource");
  const calls: { command: string; args: string[] }[] = [];
  const build: typeof buildDaemons = async (args, cwd, env) => {
    calls.push({ command: "buildDaemons", args });
    assert.deepEqual(args, ["--target", native]);
    assert.equal(cwd, workspace);
    assert.equal(env.CARGO_TARGET_DIR, cache);
    const result = { ctld: "", ctmuxd: "", "ctl-taskd": "" };
    for (const name of ["ctld", "ctmuxd", "ctl-taskd"] as const) {
      const executable = join(root, `compiled-${name}`);
      await writeFile(executable, windows ? windowsExecutableFixture(native) : `fresh ${name}`, { mode: 0o755 });
      result[name] = executable;
    }
    return result;
  };
  const run: MacosCommandRunner = async (command, args, context) => {
    calls.push({ command, args });
    const empty = { stdout: "", stderr: "" };
    if (command === "cargo" && args[0] === "metadata") {
      assert.equal(context.env.CARGO_TARGET_DIR, target_directory);
      return { stdout: JSON.stringify({ workspace_root: workspace, target_directory,
        packages: [{ name: "ctmux-app", version: "0.1.0" }] }), stderr: "" };
    }
    if (command === "fixture-rustc") return { stdout: `host: ${native}\n`, stderr: "" };
    if (command === "security") {
      assert.equal(args[args.indexOf("-i") + 1], profile);
      if (!profile_available) throw new Error("fixture profile unavailable");
      return empty;
    }
    if (command === "/usr/libexec/PlistBuddy") {
      const expiration = profile_expired ? "2000-01-01T00:00:00Z" : "2099-01-01T00:00:00Z";
      return { stdout: args[1]!.endsWith(":ExpirationDate") ? expiration :
        "TEAM123ABC.dev.tokn-ai.ctl.ctld", stderr: "" };
    }
    if (command === "xcodebuild") {
      assert.ok(settings.configured_project);
      assert.equal(args[args.indexOf("-project") + 1], provisioning_project);
      assert.ok(args.includes("-allowProvisioningUpdates"));
      assert.ok(args.includes("-allowProvisioningDeviceRegistration"));
      if (settings.renewal_failure) throw new Error("fixture Xcode provisioning refresh failed");
      profile_available = true;
      profile_expired = false;
      await mkdir(dirname(profile), { recursive: true });
      await writeFile(profile, "renewed fixture provisioning profile");
      return empty;
    }
    if (args[0] === "--component-info") {
      const component = componentMapFixture().ctld;
      if (settings.invalid_helper) component.build.version = "0.0.0";
      return { stdout: JSON.stringify(component), stderr: "" };
    }
    if (command === "/bin/sh") {
      assert.equal(context.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING, "false");
      assert.equal(context.env.CTLD_SIGNING_TIMESTAMP, "none");
      const signed = context.env.CTLD_PROVISIONING_PROFILE !== undefined;
      assert.equal(context.env.CTLD_PROVISIONING_PROFILE, signed ? profile : undefined);
      const app = args[2]!;
      await mkdir(join(app, "Contents/MacOS"), { recursive: true });
      await copyFile(args[1]!, join(app, "Contents/MacOS/ctld"));
      await writeFile(join(app, "Contents/Info.plist"), "fixture helper metadata");
      if (signed) {
        await mkdir(join(app, "Contents/_CodeSignature"));
        await writeFile(join(app, "Contents/_CodeSignature/CodeResources"), "fixture signature");
        await copyFile(profile, join(app, "Contents/embedded.provisionprofile"));
        if (settings.signing_failure) throw new Error("fixture matching signing identity unavailable");
        await writeFile(context.env.CTLD_SIGNING_IDENTITY_OUTPUT!, "A".repeat(40));
      }
      return empty;
    }
    if (command === "codesign") return args.includes("-d") ? { stdout: "", stderr: "TeamIdentifier=TEAM123ABC\n" } : empty;
    if (command === "lipo") return empty;
    if (command === "env" || command === "tar") return execute(command, args, { cwd: context.cwd, env: context.env });
    if (command === "pnpm") {
      assert.equal(context.env.CARGO_TARGET_DIR, cache);
      assert.ok(args.includes("--debug"));
      assert.equal(args[args.indexOf("--target") + 1], native);
      if (windows) {
        assert.ok(args.includes("--no-bundle"));
        assert.ok(!args.includes("--bundles"));
      } else assert.equal(args[args.indexOf("--bundles") + 1], macos ? "app" : "appimage");
      const config = JSON.parse(await readFile(args[args.indexOf("--config") + 1]!, "utf8"));
      if (windows) assert.deepEqual(config.bundle.externalBin, []);
      assert.deepEqual(config.bundle.resources, {
        [join(config_directory, "resources/agent-bundles")]: "resources/agent-bundles/",
      });
      for (const path of config.bundle.externalBin) {
        assert.ok(path.startsWith(join(output, "binaries")));
        assert.match(await readFile(`${path}-${native}`, "utf8"), /^fresh /);
      }
      await assert.rejects(readFile(windows ? windows_app : join(bundle_directory, "stale-app")));
      if (settings.output === "failed") throw new Error("fixture frontend failed");
      if (settings.output === "missing") return empty;
      await mkdir(bundle_directory, { recursive: true });
      if (windows) {
        await writeFile(windows_app, windowsExecutableFixture(native));
      } else if (macos) {
        assert.ok(args.includes("--no-sign"));
        const app = join(bundle_directory, "ctmux.app");
        await mkdir(join(app, "Contents/MacOS"), { recursive: true });
        await writeFile(join(app, "Contents/MacOS/ctmux-app"), "fresh frontend and GUI", { mode: 0o755 });
        await cp(config.bundle.macOS.files["Helpers/ctld.app"], join(app, "Contents/Helpers/ctld.app"), { recursive: true });
      } else {
        await writeFile(join(bundle_directory, "ctmux_0.1.0_amd64.AppImage"), "fresh frontend and GUI", { mode: 0o755 });
      }
      return empty;
    }
    throw new Error(`unexpected command ${command}`);
  };
  const options: DesktopBuildOptions = { workspace, output, home_directory: home,
    env: { ...process.env, RUSTC: "fixture-rustc", CARGO_TARGET_DIR: target_directory,
      CTLD_PROVISIONING_PROFILE: settings.profile_from_home ? undefined : profile } };
  return { options, run, build, calls };
}

test("local desktop builds native daemons and a fresh debug AppImage into private staging", async (t) => {
  const input = await fixture(t);
  const result = await buildDesktopApp(input.options, input.run, input.build);
  assert.equal(result.archive, "ctmux.AppImage");
  assert.equal(result.app_version, "0.1.0");
  assert.match(result.bundle_id, /^local\.[a-f0-9]{64}$/);
  assert.equal(await readFile(join(input.options.output, result.archive), "utf8"), "fresh frontend and GUI");
  assert.deepEqual(JSON.parse(await readFile(join(input.options.output, "result.json"), "utf8")), result);
  assert.deepEqual(input.calls.filter((call) => call.command === "pnpm").map((call) => call.args.slice(0, 2)), [["--workspace-root", "desktop:build"]]);
});

test("macOS reuses an available provisioning profile for its helper and leaves GUI signing optional", async (t) => {
  for (const profile of [false, true]) {
    const input = await fixture(t, { target: "aarch64-apple-darwin", profile, configured_project: profile });
    const result = await buildDesktopApp(input.options, input.run, input.build);
    assert.equal(result.archive, "ctmux.app.tar.gz");
    const listing = await execute("tar", ["-tzf", join(input.options.output, result.archive)]);
    assert.match(listing.stdout, /ctmux.app\/Contents\/Helpers\/ctld.app\/Contents\/MacOS\/ctld/);
    assert.equal(listing.stdout.includes("embedded.provisionprofile"), profile);
    assert.equal(input.calls.some((call) => call.command === "codesign"), profile);
    assert.equal(input.calls.some((call) => call.command === "xcodebuild"), false);
  }
});

test("macOS renews configured provisioning before signing an expired or missing helper profile", async (t) => {
  for (const initial_profile of ["expired", "missing"] as const) {
    const input = await fixture(t, {
      target: "aarch64-apple-darwin", configured_project: true, profile_from_home: true,
      profile: initial_profile === "expired", profile_expired: initial_profile === "expired",
    });
    const result = await buildDesktopApp(input.options, input.run, input.build);
    const archive = join(input.options.output, result.archive);
    const embedded = await execute("tar", ["-xOf", archive,
      "ctmux.app/Contents/Helpers/ctld.app/Contents/embedded.provisionprofile"]);
    assert.equal(embedded.stdout, "renewed fixture provisioning profile");
    const listing = await execute("tar", ["-tzf", archive]);
    assert.ok(listing.stdout.includes("_CodeSignature/CodeResources"));
    assert.equal(input.calls.filter((call) => call.command === "xcodebuild").length, 1);
    assert.equal(input.calls.filter((call) => call.command === "/bin/sh").length, 1);
    assert.ok(input.calls.findIndex((call) => call.command === "xcodebuild") <
      input.calls.findIndex((call) => call.command === "codesign"));
    const receipt = JSON.parse(await readFile(join(input.options.output, "helper/installation.json"), "utf8"));
    assert.equal(receipt.signing_mode, "development");
  }
});

test("failed macOS provisioning renewal falls back to a clean unsigned helper", async (t) => {
  const input = await fixture(t, {
    target: "aarch64-apple-darwin", profile: true, profile_expired: true,
    configured_project: true, renewal_failure: true, profile_from_home: true,
  });
  const result = await buildDesktopApp(input.options, input.run, input.build);
  const listing = await execute("tar", ["-tzf", join(input.options.output, result.archive)]);
  assert.ok(!listing.stdout.includes("embedded.provisionprofile"));
  assert.ok(!listing.stdout.includes("_CodeSignature"));
  assert.equal(input.calls.filter((call) => call.command === "xcodebuild").length, 1);
  assert.equal(input.calls.some((call) => call.command === "codesign"), false);
  assert.deepEqual(await readdir(join(input.options.output, "helper")), ["ctld.app"]);
});

test("failed or incomplete frontend builds cannot reuse earlier app outputs", async (t) => {
  for (const target of ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]) {
    for (const output of ["failed", "missing"] as const) {
      const input = await fixture(t, { output, target });
      await assert.rejects(buildDesktopApp(input.options, input.run, input.build));
      await assert.rejects(readFile(join(input.options.output, "result.json")));
    }
  }
});

test("Windows local builds produce complete native portable apps without launching any executable", async (t) => {
  for (const target of ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]) {
    const input = await fixture(t, { target });
    const result = await buildDesktopApp(input.options, input.run, input.build);
    assert.equal(result.archive, "ctmux.zip");
    const extracted = join(input.options.output, "extracted");
    await extractWindowsPortableFixture(join(input.options.output, result.archive), extracted);
    const app = join(extracted, "ctmux");
    const manifest = JSON.parse(await readFile(join(app, "portable.json"), "utf8"));
    assert.equal(manifest.target_triple, target);
    assert.equal(manifest.app_version, result.app_version);
    assert.deepEqual(Object.keys(manifest.files), [
      "ctl-taskd.exe", "ctld.exe", "ctmux-app.exe", "ctmuxd.exe", "resources/agent-bundles/.resource",
    ]);
    assert.equal(await readFile(join(app, "resources/agent-bundles/.resource"), "utf8"), "bundled resource");
    assert.ok(!input.calls.some((call) => call.command.endsWith(".exe") || call.args[0] === "--component-info"));
  }
});

test("a helper signing failure falls back to a clean unsigned package", async (t) => {
  const input = await fixture(t, { target: "aarch64-apple-darwin", profile: true, signing_failure: true });
  const result = await buildDesktopApp(input.options, input.run, input.build);
  const listing = await execute("tar", ["-tzf", join(input.options.output, result.archive)]);
  assert.ok(!listing.stdout.includes("embedded.provisionprofile"));
  assert.ok(!listing.stdout.includes("_CodeSignature"));
  assert.equal(input.calls.filter((call) => call.command === "/bin/sh").length, 2);
  assert.deepEqual(await readdir(join(input.options.output, "helper")), ["ctld.app"]);
});

test("invalid compiled helper metadata still fails before signing or bundling", async (t) => {
  const input = await fixture(t, { target: "aarch64-apple-darwin", profile: true, invalid_helper: true });
  await assert.rejects(buildDesktopApp(input.options, input.run, input.build), /invalid source identity or version/);
  assert.ok(!input.calls.some((call) => call.command === "/bin/sh" || call.command === "pnpm"));
});

test("version mismatches fail before compiling or staging native services", async (t) => {
  const input = await fixture(t, { mixed_version: true });
  await assert.rejects(buildDesktopApp(input.options, input.run, input.build), /same ctmux version/);
  assert.ok(!input.calls.some((call) => call.command === "buildDaemons"));
});

test("Windows pnpm shims receive paths containing spaces and shell characters literally", { skip: process.platform !== "win32" }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ctl-desktop-command-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const script = join(root, "arguments.cjs");
  await writeFile(script, "process.stdout.write(JSON.stringify(process.argv.slice(2)))");
  await writeFile(join(root, "pnpm.cmd"), '@"%CTL_TEST_NODE%" "%CTL_TEST_SCRIPT%" %*\r\n');
  const args = ["--config", join(root, "space & percent% ! (config).json")];
  const result = await runDesktopBuildCommand("pnpm", args, {
    cwd: root, env: { ...process.env, PATH: `${root};${process.env.PATH ?? process.env.Path ?? ""}`,
      CTL_TEST_NODE: process.execPath, CTL_TEST_SCRIPT: script },
  });
  assert.deepEqual(JSON.parse(result.stdout), args);
});
