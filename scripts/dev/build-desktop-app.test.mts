import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, cp, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { componentMapFixture } from "../ci/agent-bundle.fixtures.mts";
import { buildDesktopApp, type DesktopBuildOptions } from "./build-desktop-app.mts";
import type { buildDaemons } from "./daemon-build.mts";
import type { MacosCommandRunner } from "./macos-provisioning.mts";

const execute = promisify(execFile);

async function fixture(t: TestContext, settings: {
  target?: string; profile?: boolean; output?: "missing" | "failed"; mixed_version?: boolean;
  signing_failure?: boolean; invalid_helper?: boolean;
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
  const profile = join(root, "profile");
  if (settings.profile) await writeFile(profile, "fixture provisioning profile");
  const checkout = createHash("sha256").update(workspace).digest("hex").slice(0, 20);
  const cache = join(target_directory, "ctl-desktop-build", checkout, "cargo");
  const macos = native.endsWith("-apple-darwin");
  const bundle_directory = join(cache, native, "debug/bundle", macos ? "macos" : "appimage");
  await mkdir(bundle_directory, { recursive: true, mode: 0o700 });
  await writeFile(join(bundle_directory, "stale-app"), "earlier build");
  const calls: { command: string; args: string[] }[] = [];
  const build: typeof buildDaemons = async (args, cwd, env) => {
    calls.push({ command: "buildDaemons", args });
    assert.deepEqual(args, ["--target", native]);
    assert.equal(cwd, workspace);
    assert.equal(env.CARGO_TARGET_DIR, cache);
    const result = { ctld: "", ctmuxd: "", "ctl-taskd": "" };
    for (const name of ["ctld", "ctmuxd", "ctl-taskd"] as const) {
      const executable = join(root, `compiled-${name}`);
      await writeFile(executable, `fresh ${name}`, { mode: 0o755 });
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
      if (!settings.profile) throw new Error("fixture profile unavailable");
      return empty;
    }
    if (command === "/usr/libexec/PlistBuddy") {
      return { stdout: args[1]!.endsWith(":ExpirationDate") ? "2099-01-01T00:00:00Z" :
        "TEAM123ABC.dev.tokn-ai.ctl.ctld", stderr: "" };
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
      assert.equal(args[args.indexOf("--bundles") + 1], macos ? "app" : "appimage");
      const config = JSON.parse(await readFile(args[args.indexOf("--config") + 1]!, "utf8"));
      assert.deepEqual(config.bundle.resources, {
        [join(config_directory, "resources/agent-bundles")]: "resources/agent-bundles/",
      });
      for (const path of config.bundle.externalBin) {
        assert.ok(path.startsWith(join(output, "binaries")));
        assert.match(await readFile(`${path}-${native}`, "utf8"), /^fresh /);
      }
      await assert.rejects(readFile(join(bundle_directory, "stale-app")));
      if (settings.output === "failed") throw new Error("fixture frontend failed");
      if (settings.output === "missing") return empty;
      await mkdir(bundle_directory, { recursive: true });
      if (macos) {
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
    env: { ...process.env, RUSTC: "fixture-rustc", CARGO_TARGET_DIR: target_directory, CTLD_PROVISIONING_PROFILE: profile } };
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
    const input = await fixture(t, { target: "aarch64-apple-darwin", profile });
    const result = await buildDesktopApp(input.options, input.run, input.build);
    assert.equal(result.archive, "ctmux.app.tar.gz");
    const listing = await execute("tar", ["-tzf", join(input.options.output, result.archive)]);
    assert.match(listing.stdout, /ctmux.app\/Contents\/Helpers\/ctld.app\/Contents\/MacOS\/ctld/);
    assert.equal(listing.stdout.includes("embedded.provisionprofile"), profile);
    assert.equal(input.calls.some((call) => call.command === "codesign"), profile);
  }
});

test("failed or incomplete frontend builds cannot reuse earlier app outputs", async (t) => {
  for (const output of ["failed", "missing"] as const) {
    const input = await fixture(t, { output });
    await assert.rejects(buildDesktopApp(input.options, input.run, input.build));
    await assert.rejects(readFile(join(input.options.output, "result.json")));
  }
});

test("a helper signing failure falls back to a clean unsigned package", async (t) => {
  const input = await fixture(t, { target: "aarch64-apple-darwin", profile: true, signing_failure: true });
  const result = await buildDesktopApp(input.options, input.run, input.build);
  const listing = await execute("tar", ["-tzf", join(input.options.output, result.archive)]);
  assert.ok(!listing.stdout.includes("embedded.provisionprofile"));
  assert.ok(!listing.stdout.includes("_CodeSignature"));
  assert.equal(input.calls.filter((call) => call.command === "/bin/sh").length, 2);
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
