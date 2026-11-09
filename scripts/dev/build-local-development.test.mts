import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, delimiter, join } from "node:path";
import test, { type TestContext } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { componentMapFixture } from "../ci/agent-bundle.fixtures.mts";
import { agentCargoPackages, agentComponents } from "../shared/agent-bundle.mts";
import { buildLocalDevelopment, type LocalDevelopmentOptions } from "./build-local-development.mts";
import type { MacosCommandContext, MacosCommandRunner } from "./macos-provisioning.mts";

const execute = promisify(execFile);
const names = [...agentComponents, "ctlup"] as const;

async function fixture(t: TestContext, settings: { host?: string; target?: string; native_target?: string; mixed?: boolean; missing?: boolean; failed?: boolean } = {}) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "ctlup-native-builder-test-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  const workspace = join(root, "workspace"), output = join(root, "output"), home = join(root, "home");
  const cargo_cwd = join(workspace, "apps/desktop/src-tauri"), target_directory = join(root, "custom Cargo cache");
  for (const directory of [workspace, output, home]) await mkdir(directory, { mode: 0o700 });
  await mkdir(cargo_cwd, { recursive: true });
  const profile = join(root, "profile");
  await writeFile(profile, "fixture provisioned profile");
  const host = settings.host ?? "x86_64-unknown-linux-gnu", target = settings.target ?? host;
  const checkout_id = createHash("sha256").update(workspace).digest("hex").slice(0, 20);
  const dedicated = join(target_directory, "ctl-dev", checkout_id, "cargo");
  const components = componentMapFixture();
  for (const info of Object.values(components)) info.build.dirty = true;
  if (settings.mixed) components["ctl-taskd"].build.source_fingerprint = "b".repeat(64);
  let stripped = false, changed_originals = false;
  const original_paths: string[] = [];
  const calls: { command: string; args: string[]; context: MacosCommandContext }[] = [];
  const run: MacosCommandRunner = async (command, args, context) => {
    calls.push({ command, args, context });
    const empty = { stdout: "", stderr: "" };
    if (command === "fixture-rustc") return { stdout: `host: ${host}\n`, stderr: "" };
    if (command === "cargo" && args[0] === "metadata") {
      assert.equal(context.cwd, cargo_cwd);
      assert.ok(args.includes(`build.target-dir=${JSON.stringify(target_directory)}`));
      return { stdout: JSON.stringify({ workspace_root: workspace, target_directory,
        packages: agentCargoPackages.map((name) => ({ name, version: "0.1.0" })) }), stderr: "" };
    }
    if (command === "cargo" && args[0] === "build") {
      assert.equal(context.cwd, cargo_cwd);
      assert.equal(args[args.indexOf("--target-dir") + 1], dedicated);
      assert.ok(args.includes("--locked"));
      assert.equal(context.env.CTL_BUNDLED_CTLD_DIR, undefined);
      const directory = join(dedicated, target, "custom-profile");
      await mkdir(directory, { recursive: true });
      const messages = [];
      for (const name of names) {
        const executable = join(directory, name);
        await writeFile(executable, `compiled ${name}`, { mode: 0o755 });
        original_paths.push(executable);
        if (settings.missing && name === "ctlup") continue;
        messages.push(JSON.stringify({ reason: "compiler-artifact", target: { name, kind: ["bin"] }, executable }));
      }
      if (settings.failed) throw new Error("fixture compiler failed after emitting artifacts");
      return { stdout: messages.join("\n"), stderr: "" };
    }
    if (command === "strip") {
      assert.equal(args.length, names.length);
      for (const path of args) {
        assert.ok(path.startsWith(join(output, "bin")));
        await writeFile(path, `${await readFile(path, "utf8")}:stripped`);
      }
      stripped = true;
      return empty;
    }
    if (args[0] === "--component-info") {
      assert.ok(stripped, "metadata describes the stripped snapshot");
      assert.deepEqual((await readdir(join(output, "bin"))).sort(), [...names].sort());
      assert.ok((await readFile(command, "utf8")).endsWith(":stripped"));
      if (!changed_originals) {
        changed_originals = true;
        for (const path of original_paths) await writeFile(path, "concurrent Cargo replacement");
      }
      const name = basename(command) === ".ctld-input" ? "ctld" : basename(command);
      return { stdout: JSON.stringify(components[name === "ctlup" ? "ctl" : name as keyof typeof components]), stderr: "" };
    }
    if (command === "security") return empty;
    if (command === "/usr/libexec/PlistBuddy") return { stdout: args[1]?.endsWith(":ExpirationDate") ? "2099-01-01T00:00:00Z" : "TEAM123ABC.dev.tokn-ai.ctl.ctld", stderr: "" };
    if (command === "/bin/sh") {
      assert.ok(stripped, "strip precedes codesigning");
      assert.equal(context.env.CTLD_SIGNING_TIMESTAMP, "none");
      assert.equal(context.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING, "false");
      const app = args[2]!;
      await mkdir(join(app, "Contents/MacOS"), { recursive: true });
      await mkdir(join(app, "Contents/_CodeSignature"));
      await copyFile(args[1]!, join(app, "Contents/MacOS/ctld"));
      await chmod(join(app, "Contents/MacOS/ctld"), 0o700);
      await copyFile(profile, join(app, "Contents/embedded.provisionprofile"));
      await writeFile(join(app, "Contents/Info.plist"), "fixture plist");
      await writeFile(join(app, "Contents/_CodeSignature/CodeResources"), "fixture seal");
      await writeFile(context.env.CTLD_SIGNING_IDENTITY_OUTPUT!, "A".repeat(40));
      return empty;
    }
    if (command === "lipo") {
      assert.equal(args[2], target.startsWith("aarch64") ? "arm64" : "x86_64");
      return empty;
    }
    if (command === "codesign") return args.includes("-d") ? { stdout: "", stderr: "TeamIdentifier=TEAM123ABC\n" } : empty;
    if (command === "env") return execute(command, args);
    throw new Error(`unexpected fixture command: ${command}`);
  };
  const options: LocalDevelopmentOptions = { workspace, output, cargo_cwd, native_target: settings.native_target,
    cargo_args: ["run", "--manifest-path", "Cargo.toml", "--target-dir", target_directory, "--profile", "custom-profile", "--target", target,
      "--config", "config with spaces.toml", "--features", "app-only", "--bin", "desktop", "--", "--app-only"],
    env: { ...process.env, HOME: home, CARGO_TARGET_DIR: "inherited Cargo target", RUSTC: "fixture-rustc", CTLD_PROVISIONING_PROFILE: profile,
      CTL_BUNDLED_CTLD_DIR: "unrelated release payload", CTLD_SIGNING_TIMESTAMP: "secure" } };
  return { root, workspace, output, home, cargo_cwd, target_directory, dedicated, calls, original_paths, run, options };
}

test("one native build snapshots all runtime commands and ctlup from Cargo's actual custom target/profile outputs", async (t) => {
  const input = await fixture(t, { target: "x86_64-unknown-linux-musl" });
  const result = await buildLocalDevelopment(input.options, input.run);
  assert.equal(result.target_triple, "x86_64-unknown-linux-musl");
  assert.equal(result.ctld_package, null);
  assert.deepEqual(JSON.parse(await readFile(join(input.output, "result.json"), "utf8")), result);
  assert.equal(input.calls.filter((call) => call.command === "cargo" && call.args[0] === "build").length, 1);
  const build = input.calls.find((call) => call.command === "cargo" && call.args[0] === "build")!;
  assert.ok(build.args.includes("custom-profile"));
  assert.ok(build.args.includes("config with spaces.toml"));
  assert.ok(!build.args.includes("app-only"));
  assert.deepEqual(build.args.flatMap((arg, index) => arg === "-p" ? [build.args[index + 1]] : []), [...agentCargoPackages]);
  for (const name of names) assert.equal(await readFile(join(result.binary_directory, name), "utf8"), `compiled ${name}:stripped`);
  for (const path of input.original_paths) assert.equal(await readFile(path, "utf8"), "concurrent Cargo replacement");
  assert.deepEqual(await readdir(input.home), []);
  await assert.rejects(lstat(join(input.target_directory, "ctl-dev/helpers")), { code: "ENOENT" });
});

test("an executable Darwin target is stripped before the existing signing recipe and emits a native-verifier receipt", async (t) => {
  const input = await fixture(t, { host: "aarch64-apple-darwin", target: "x86_64-apple-darwin", native_target: "x86_64-apple-darwin" });
  const result = await buildLocalDevelopment(input.options, input.run);
  assert.equal(result.target_triple, "x86_64-apple-darwin");
  assert.equal(result.ctld_package, join(input.output, "helper"));
  const receipt = JSON.parse(await readFile(join(result.ctld_package!, "installation.json"), "utf8"));
  assert.equal(receipt.target, result.target_triple);
  assert.equal(receipt.signing_mode, "development");
  assert.equal(receipt.development.dirty, true);
  assert.deepEqual((await readdir(result.ctld_package!)).sort(), ["ctld.app", "installation.json"]);
  assert.equal(await readFile(join(result.ctld_package!, "ctld.app/Contents/MacOS/ctld"), "utf8"), "compiled ctld:stripped");
  assert.equal(input.calls.filter((call) => call.command === "cargo" && call.args[0] === "build").length, 1);
  assert.equal(input.calls.filter((call) => call.command === "/bin/sh").length, 1);
  assert.deepEqual(await readdir(input.home), []);
});

for (const [name, settings] of [
  ["mixed source identities", { mixed: true }], ["missing updater artifact", { missing: true }],
  ["compiler failure after artifacts", { failed: true }], ["foreign target", { target: "x86_64-pc-windows-msvc" }],
  ["unrelated Linux CPU", { target: "aarch64-unknown-linux-musl" }],
  ["unrelated Darwin CPU", { host: "aarch64-apple-darwin", target: "x86_64-apple-darwin" }],
  ["GNU binaries for a musl installation", { host: "x86_64-unknown-linux-musl", target: "x86_64-unknown-linux-gnu" }],
] as const) {
  test(`${name} never publishes a successful result or changes an account installation`, async (t) => {
    const input = await fixture(t, settings);
    await assert.rejects(buildLocalDevelopment(input.options, input.run));
    await assert.rejects(readFile(join(input.output, "result.json")), { code: "ENOENT" });
    assert.deepEqual(await readdir(input.home), []);
    assert.equal(input.calls.some((call) => call.command === "/bin/sh"), false);
    if ("target" in settings) assert.equal(input.calls.some((call) => call.command === "strip"), false);
  });
}

test("closing the owning ctlup lifeline stops the builder's Cargo process and compiler descendants", { skip: process.platform === "win32", timeout: 15_000 }, async (t) => {
  const input = await fixture(t);
  const tools = join(input.root, "tools"), pidfile = join(input.root, "pids.json");
  await mkdir(tools);
  const metadata = JSON.stringify({ workspace_root: input.workspace, target_directory: input.target_directory, packages: [{ name: "ctld", version: "0.1.0" }] });
  await writeFile(join(tools, "cargo"), `#!${process.execPath}\nconst fs=require('node:fs');const{spawn}=require('node:child_process');if(process.argv[2]==='metadata')console.log(${JSON.stringify(metadata)});else{const compiler=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});fs.writeFileSync(process.env.CTLUP_TEST_PIDFILE,JSON.stringify([process.pid,compiler.pid]));setInterval(()=>{},1000);}\n`, { mode: 0o700 });
  await writeFile(join(tools, "rustc"), `#!${process.execPath}\nconsole.log('host: x86_64-unknown-linux-gnu');\n`, { mode: 0o700 });
  const child = spawn(process.execPath, ["--experimental-strip-types", "--disable-warning=ExperimentalWarning",
    fileURLToPath(new URL("./build-local-development.mts", import.meta.url)), "--workspace", input.workspace,
    "--output", input.output, "--cargo-args-json", "[]", "--cargo-cwd", input.cargo_cwd], {
    cwd: input.cargo_cwd, detached: true, stdio: ["pipe", "ignore", "pipe"],
    env: { ...process.env, PATH: `${tools}${delimiter}${process.env.PATH ?? ""}`, RUSTC: "rustc",
      CTLUP_DEVELOPMENT_LIFELINE: "1", CTLUP_TEST_PIDFILE: pidfile },
  });
  const closed = once(child, "close");
  let pids: number[] = [];
  try {
    for (let attempt = 0; attempt < 200; attempt += 1) {
      try { pids = JSON.parse(await readFile(pidfile, "utf8")); break; } catch { await delay(10); }
    }
    assert.equal(pids.length, 2, "Cargo and its compiler are running");
    child.stdin!.destroy();
    const [, signal] = await closed;
    assert.equal(signal, "SIGKILL");
    for (const pid of pids) {
      let stopped = false;
      for (let attempt = 0; attempt < 200; attempt += 1) {
        try { process.kill(pid, 0); } catch (error) { if ((error as NodeJS.ErrnoException).code === "ESRCH") { stopped = true; break; } }
        await delay(10);
      }
      assert.equal(stopped, true, "owned build descendant stopped after the lifeline closed");
    }
    await assert.rejects(readFile(join(input.output, "result.json")), { code: "ENOENT" });
  } finally {
    child.stdin!.destroy();
    try { process.kill(-child.pid!, "SIGKILL"); } catch {}
    for (const pid of pids) { try { process.kill(pid, "SIGKILL"); } catch {} }
    await closed;
  }
});
