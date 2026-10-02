import assert from "node:assert/strict";
import { execFile as execFileCallback } from "node:child_process";
import { chmod, copyFile, lstat, mkdir, readFile, readlink, stat, symlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { fixture, packages, running, startExternal, starts, stopFixture, supervisor, unixOnly, waitReady, writeHelper } from "./signed-daemon.fixtures.mts";
import { SignedDaemon } from "./signed-daemon.mts";

const execFile = promisify(execFileCallback);

test("Tauri development packaging explicitly disables inherited release timestamp policy", unixOnly, async (context) => {
  const data = await fixture(context);
  const script = path.join(data.root, "scripts/ci/package-ctld-app.sh");
  const environment_log = path.join(data.root, "packaging-environment.json");
  await writeFile(script, `#!${process.execPath}
const fs = require("node:fs");
const path = require("node:path");
fs.writeFileSync(${JSON.stringify(environment_log)}, JSON.stringify({
  timestamp: process.env.CTLD_SIGNING_TIMESTAMP,
  distribution: process.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING,
  profile: fs.readFileSync(process.env.CTLD_PROVISIONING_PROFILE, "utf8")
}));
const executable = path.join(process.argv[3], "Contents/MacOS/ctld");
fs.mkdirSync(path.dirname(executable), { recursive: true });
fs.copyFileSync(process.argv[2], executable);
`);
  await chmod(script, 0o700);
  await data.supervisor.close();
  data.supervisor = new SignedDaemon(data.config, { startup_timeout_ms: 800, shutdown_timeout_ms: 200 });
  const previous_timestamp = process.env.CTLD_SIGNING_TIMESTAMP;
  const previous_distribution = process.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING;
  process.env.CTLD_SIGNING_TIMESTAMP = "secure";
  process.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING = "true";
  try {
    await data.supervisor.prepare(data.executable);
  } finally {
    if (previous_timestamp === undefined) delete process.env.CTLD_SIGNING_TIMESTAMP;
    else process.env.CTLD_SIGNING_TIMESTAMP = previous_timestamp;
    if (previous_distribution === undefined) delete process.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING;
    else process.env.CTLD_REQUIRE_DISTRIBUTION_SIGNING = previous_distribution;
  }
  assert.deepEqual(JSON.parse(await readFile(environment_log, "utf8")), {
    timestamp: "none", distribution: "false", profile: "test provisioning profile",
  });
  await waitReady(data, 5);
});

test("reuses a signed daemon across reloads and fresh supervisors", unixOnly, async (context) => {
  const data = await fixture(context);
  await Promise.all(Array.from({ length: 3 }, () => data.supervisor.prepare(data.executable)));
  await data.supervisor.close();
  const reopened = supervisor(data);
  await reopened.prepare(data.executable);
  await reopened.close();
  assert.equal(await packages(data), 1);
  const processes = await starts(data);
  assert.equal(processes.length, 1);
  assert.equal(running(processes[0].pid), true);
  assert.equal((await stat(data.supervisor.socket_path)).isSocket(), true);
  assert.deepEqual(data.diagnostics, []);
});

for (const version of [5, 6]) {
  test(`stages a changed helper without replacing live protocol 5 (new protocol ${version})`, unixOnly, async (context) => {
    const data = await fixture(context);
    await data.supervisor.prepare(data.executable);
    const originalLink = await readlink(path.join(data.root, "runtime/ctld.app"));
    const [original] = await starts(data);
    const originalExecutable = path.join(originalLink, "Contents/MacOS/ctld");
    assert.equal(original.ctld_bin, originalExecutable);
    await writeHelper(data, version, "changed_build");
    await data.supervisor.prepare(data.executable);
    assert.equal((await starts(data)).length, 1);
    assert.equal(running(original.pid), true);
    assert.equal(await packages(data), 2);
    assert.notEqual(await readlink(path.join(data.root, "runtime/ctld.app")), originalLink);
    assert.equal((await stat(path.join(originalLink, "Contents/MacOS/ctld"))).isFile(), true);
    const { stdout } = await execFile(original.ctld_bin!, ["--protocol-version"]);
    assert.equal(stdout.trim(), "5", "retained daemon's child helper must remain on its original protocol");
    assert.equal(data.diagnostics.length, 1);
    assert.match(data.diagnostics[0], version === 5 ? /New signed helper staged/ : /still using protocol 5/);
    await data.supervisor.prepare(data.executable);
    assert.equal(data.diagnostics.length, 1, "unchanged reload must not repeat the diagnostic");
    await waitReady(data, 5);
  });
}

test("failed startup preserves a different daemon that won the endpoint", unixOnly, async (context) => {
  const data = await fixture(context);
  await writeHelper(data, 6);
  await copyFile(data.executable, data.executable + ".replacement");
  await writeHelper(data, 5, "replacement_race");
  await assert.rejects(data.supervisor.prepare(data.executable), /unexpected protocol version/);
  const [attempt, replacement] = await starts(data);
  assert.equal(running(attempt.pid), false);
  assert.equal(running(replacement.pid), true);
  await waitReady(data, 6);
});

test("a signing failure preserves the live daemon and selected helper", unixOnly, async (context) => {
  const data = await fixture(context);
  await data.supervisor.prepare(data.executable);
  const originalLink = await readlink(path.join(data.root, "runtime/ctld.app"));
  const [original] = await starts(data);
  await writeHelper(data, 6);
  data.fail_signing = true;
  await assert.rejects(data.supervisor.prepare(data.executable), /test signing failed/);
  assert.equal(running(original.pid), true);
  assert.equal(await readlink(path.join(data.root, "runtime/ctld.app")), originalLink);
  data.fail_signing = false;
  await data.supervisor.prepare(data.executable);
  assert.equal(running(original.pid), true);
  assert.equal((await starts(data)).length, 1);
});

test("invalidates the persistent cache when the signing recipe changes", unixOnly, async (context) => {
  const data = await fixture(context);
  await data.supervisor.prepare(data.executable);
  const originalLink = await readlink(path.join(data.root, "runtime/ctld.app"));
  const [original] = await starts(data);
  await data.supervisor.close();
  await writeFile(path.join(data.root, "apps/desktop/src-tauri/macos/ctld/Entitlements.plist"), "updated test signing recipe");
  const reopened = supervisor(data);
  await reopened.prepare(data.executable);
  await reopened.prepare(data.executable);
  await reopened.close();
  assert.equal(await packages(data), 2);
  assert.notEqual(await readlink(path.join(data.root, "runtime/ctld.app")), originalLink);
  assert.equal((await starts(data)).length, 1);
  assert.equal(running(original.pid), true);
});

for (const mode of ["mismatch", "hang", "exit"]) {
  test(`terminates only the newly spawned daemon after readiness failure (${mode})`, unixOnly, async (context) => {
    const data = await fixture(context);
    await writeHelper(data, 6, mode);
    await assert.rejects(data.supervisor.prepare(data.executable), /invalid lifecycle response|query in time|stopped during startup/);
    const [child] = await starts(data);
    assert.equal(running(child.pid), false);
    await assert.rejects(stat(data.supervisor.socket_path), { code: "ENOENT" });
    await writeHelper(data, 6);
    await data.supervisor.prepare(data.executable);
    assert.equal((await starts(data)).length, 2);
  });
}

test("close drains pending preparation and preserves the daemon and bundle", unixOnly, async (context) => {
  const data = await fixture(context);
  const preparation = data.supervisor.prepare(data.executable);
  const closing = data.supervisor.close();
  await preparation;
  await closing;
  const [child] = await starts(data);
  assert.equal(running(child.pid), true);
  assert.equal((await stat(data.supervisor.socket_path)).isSocket(), true);
  assert.equal((await stat(data.supervisor.executable)).isFile(), true);
  await waitReady(data, 5);
  await assert.rejects(data.supervisor.prepare(data.executable), /supervisor is closing/);
});

test("reuses the replacement after an explicit external restart", unixOnly, async (context) => {
  const data = await fixture(context);
  await data.supervisor.prepare(data.executable);
  const [original] = await starts(data);
  await writeHelper(data, 6);
  await data.supervisor.prepare(data.executable);
  await stopFixture(original.pid);
  const replacement = await startExternal(data, data.supervisor.executable);
  await waitReady(data, 6);
  const reopened = supervisor(data);
  await reopened.prepare(data.executable);
  await reopened.close();
  assert.deepEqual((await starts(data)).map((item) => item.pid), [original.pid, replacement]);
  assert.equal(await packages(data), 2);
  assert.equal(running(replacement), true);
});

test("allows ctld to recover its own stale endpoint after a crash", unixOnly, async (context) => {
  const data = await fixture(context);
  await data.supervisor.prepare(data.executable);
  const [original] = await starts(data);
  await stopFixture(original.pid, "SIGKILL");
  assert.equal((await stat(data.supervisor.socket_path)).isSocket(), true);
  await data.supervisor.prepare(data.executable);
  await waitReady(data, 5);
  assert.equal((await starts(data)).length, 2);
  assert.equal(await packages(data), 1);
});

for (const mode of ["mismatch", "hang"]) {
  test(`leaves an existing ${mode} owner untouched`, unixOnly, async (context) => {
    const data = await fixture(context);
    await mkdir(data.config.runtime_directory, { mode: 0o700 });
    await writeHelper(data, 5, mode);
    const existing = await startExternal(data);
    // This owner intentionally never returns a valid lifecycle response.
    for (let attempt = 0; attempt < 100; attempt += 1) {
      if (await lstat(data.supervisor.socket_path).then(() => true, () => false)) break;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    const before = await lstat(data.supervisor.socket_path);
    await writeHelper(data, 6);
    await assert.rejects(data.supervisor.prepare(data.executable), /invalid lifecycle response|query in time/);
    assert.equal((await starts(data)).length, 1);
    assert.equal(running(existing), true);
    assert.equal((await lstat(data.supervisor.socket_path)).ino, before.ino);
  });
}

for (const kind of ["file", "symlink"]) {
  test(`rejects an unexpected endpoint ${kind} without deleting it`, unixOnly, async (context) => {
    const data = await fixture(context);
    await mkdir(data.config.runtime_directory, { mode: 0o700 });
    if (kind === "file") await writeFile(data.supervisor.socket_path, "keep");
    else await symlink(data.executable, data.supervisor.socket_path);
    const before = await lstat(data.supervisor.socket_path);
    await assert.rejects(data.supervisor.prepare(data.executable), /not an owned Unix socket/);
    assert.equal((await lstat(data.supervisor.socket_path)).ino, before.ino);
    assert.deepEqual(await starts(data), []);
  });
}

test("concurrent independent launchers serialize preparation and exit while ctld survives", unixOnly, async (context) => {
  const data = await fixture(context);
  const worker = path.join(data.root, "launcher.mts");
  const manager = fileURLToPath(new URL("./signed-daemon.mts", import.meta.url));
  await writeFile(worker, `
import { appendFile, copyFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { SignedDaemon } from ${JSON.stringify(manager)};
const daemon = new SignedDaemon(${JSON.stringify(data.config)}, {
  package_bundle: async (input, bundle) => {
    await appendFile(${JSON.stringify(data.package_log)}, "package\\n");
    await new Promise((resolve) => setTimeout(resolve, 50));
    const output = path.join(bundle, "Contents/MacOS/ctld");
    await mkdir(path.dirname(output), {recursive: true});
    await copyFile(input, output);
  }
});
await daemon.prepare(${JSON.stringify(data.executable)});
await daemon.close();
`);
  const launch = () => execFile(process.execPath, ["--experimental-strip-types", "--disable-warning=ExperimentalWarning", worker], { timeout: 8_000 });
  await Promise.all([launch(), launch(), launch()]);
  const [original] = await starts(data);
  assert.equal(await packages(data), 1);
  assert.equal((await starts(data)).length, 1);
  assert.equal(running(original.pid), true);
  await waitReady(data, 5);
  await launch();
  assert.equal((await starts(data)).length, 1);
  assert.equal(await packages(data), 1);
  assert.equal(running(original.pid), true);
  assert.equal((await readFile(data.package_log, "utf8")).trim(), "package");
});
