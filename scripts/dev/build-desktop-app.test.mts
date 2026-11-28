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
import type { BuildCommandRunner } from "./build-command.mts";

const execute = promisify(execFile);

async function fixture(t: TestContext, settings: {
  target?: string; output?: "missing" | "failed"; mixed_version?: boolean;
  signing?: "unavailable" | "partial" | "skip"; invalid_helper?: boolean;
} = {}) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "ctl-desktop-source-test-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  const workspace = join(root, "workspace"), output = join(root, "output"), signer = join(root, "fixture ctlup");
  const target_directory = join(root, "custom Cargo cache"), native = settings.target ?? "x86_64-unknown-linux-gnu";
  const config_directory = join(workspace, "apps/desktop/src-tauri");
  for (const directory of [workspace, output]) await mkdir(directory, { mode: 0o700 });
  await mkdir(config_directory, { recursive: true });
  await mkdir(join(config_directory, "resources/agent-bundles"), { recursive: true });
  await writeFile(join(config_directory, "tauri.conf.json"), JSON.stringify({
    productName: "ctmux", version: settings.mixed_version ? "0.1.1" : "0.1.0",
  }));
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
  const run: BuildCommandRunner = async (command, args, context) => {
    calls.push({ command, args });
    const empty = { stdout: "", stderr: "" };
    if (command === "cargo" && args[0] === "metadata") {
      assert.equal(context.env.CARGO_TARGET_DIR, target_directory);
      return { stdout: JSON.stringify({ workspace_root: workspace, target_directory,
        packages: [{ name: "ctmux-app", version: "0.1.0" }] }), stderr: "" };
    }
    if (command === "fixture-rustc") return { stdout: `host: ${native}\n`, stderr: "" };
    if (args[0] === "--component-info") {
      const component = componentMapFixture().ctld;
      if (settings.invalid_helper) component.build.version = "0.0.0";
      return { stdout: JSON.stringify(component), stderr: "" };
    }
    if (command === signer) {
      const artifact = join(output, "binaries", `ctld-${native}`), helper = join(output, "helper");
      assert.deepEqual(args, ["sign-development-helper", "--artifact", artifact,
        "--output", helper, "--version", "0.1.0", "--non-interactive"]);
      if (settings.signing === "unavailable") throw new Error("fixture provisioning unavailable");
      const app = join(helper, "ctld.app");
      await mkdir(join(app, "Contents/MacOS"), { recursive: true });
      await mkdir(join(app, "Contents/_CodeSignature"));
      await copyFile(artifact, join(app, "Contents/MacOS/ctld"));
      await writeFile(join(app, "Contents/Info.plist"), "fixture helper metadata");
      await writeFile(join(app, "Contents/embedded.provisionprofile"), "fixture profile");
      await writeFile(join(app, "Contents/_CodeSignature/CodeResources"), "fixture signature");
      await writeFile(join(helper, "installation.json"), JSON.stringify({ signing_mode: "development" }));
      if (settings.signing === "partial") throw new Error("fixture signing failed after staging");
      return empty;
    }
    if (command === "/bin/sh") {
      assert.equal(context.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING, "false");
      assert.equal(context.env.CTLD_SIGNING_TIMESTAMP, "none");
      assert.equal(context.env.CTLD_PROVISIONING_PROFILE, undefined);
      const app = args[2]!;
      await mkdir(join(app, "Contents/MacOS"), { recursive: true });
      await copyFile(args[1]!, join(app, "Contents/MacOS/ctld"));
      await writeFile(join(app, "Contents/Info.plist"), "fixture helper metadata");
      return empty;
    }
    if (command === "tar") return execute(command, args, { cwd: context.cwd, env: context.env });
    if (command === "pnpm") {
      assert.equal(context.env.CARGO_TARGET_DIR, cache);
      assert.equal(context.env.CTL_DESKTOP_BUILD_CHANNEL, "local");
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
  const options: DesktopBuildOptions = { workspace, output,
    env: { ...process.env, RUSTC: "fixture-rustc", CARGO_TARGET_DIR: target_directory, CTL_DESKTOP_BUILD_CHANNEL: "nightly",
      CTLUP_DEVELOPMENT_EXECUTABLE: signer, CTLUP_SKIP_DEVELOPMENT_SIGNING: settings.signing === "skip" ? "1" : undefined } };
  return { options, run, build, calls, signer };
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

test("macOS delegates helper signing to ctlup and leaves GUI signing optional", async (t) => {
  for (const signing of [undefined, "unavailable", "skip"] as const) {
    const input = await fixture(t, { target: "aarch64-apple-darwin", signing });
    const result = await buildDesktopApp(input.options, input.run, input.build);
    assert.equal(result.archive, "ctmux.app.tar.gz");
    const listing = await execute("tar", ["-tzf", join(input.options.output, result.archive)]);
    assert.match(listing.stdout, /ctmux.app\/Contents\/Helpers\/ctld.app\/Contents\/MacOS\/ctld/);
    assert.equal(listing.stdout.includes("embedded.provisionprofile"), signing === undefined);
    assert.equal(input.calls.filter((call) => call.command === input.signer).length, signing === "skip" ? 0 : 1);
    assert.equal(input.calls.some((call) => call.command === "/bin/sh"), signing !== undefined);
  }
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
  const input = await fixture(t, { target: "aarch64-apple-darwin", signing: "partial" });
  const result = await buildDesktopApp(input.options, input.run, input.build);
  const listing = await execute("tar", ["-tzf", join(input.options.output, result.archive)]);
  assert.ok(!listing.stdout.includes("embedded.provisionprofile"));
  assert.ok(!listing.stdout.includes("_CodeSignature"));
  assert.equal(input.calls.filter((call) => call.command === input.signer).length, 1);
  assert.equal(input.calls.filter((call) => call.command === "/bin/sh").length, 1);
  assert.deepEqual(await readdir(join(input.options.output, "helper")), ["ctld.app"]);
});

test("invalid compiled helper metadata still fails before signing or bundling", async (t) => {
  const input = await fixture(t, { target: "aarch64-apple-darwin", invalid_helper: true });
  await assert.rejects(buildDesktopApp(input.options, input.run, input.build), /invalid source identity or version/);
  assert.ok(!input.calls.some((call) => call.command === input.signer || call.command === "/bin/sh" || call.command === "pnpm"));
});

test("version mismatches fail before compiling or staging native services", async (t) => {
  const input = await fixture(t, { mixed_version: true });
  await assert.rejects(buildDesktopApp(input.options, input.run, input.build), /same ctmux version/);
  assert.ok(!input.calls.some((call) => call.command === "buildDaemons"));
});

test("Windows desktop commands receive paths containing spaces and shell characters literally", { skip: process.platform !== "win32" }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ctl desktop & command-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const app_directory = join(root, "apps/desktop");
  const script = join(app_directory, "node_modules/@tauri-apps/cli/tauri.js");
  await mkdir(dirname(script), { recursive: true });
  await writeFile(script, "process.stdout.write(JSON.stringify({ args: process.argv.slice(2), cwd: process.cwd() }))");
  const args = ["--workspace-root", "desktop:build", "--config", join(root, "space & percent% ! (config).json")];
  const result = await runDesktopBuildCommand("pnpm", args, { cwd: root, env: process.env });
  const actual = JSON.parse(result.stdout) as { args: string[]; cwd: string };
  assert.deepEqual(actual.args, ["build", ...args.slice(2)]);
  assert.equal(await realpath(actual.cwd), await realpath(app_directory));
});
