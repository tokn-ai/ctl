import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { test, type TestContext } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { componentMapFixture } from "../ci/agent-bundle.fixtures.mts";
import { agentComponents } from "../shared/agent-bundle.mts";
import { readElfComponent } from "../shared/elf-component.mts";
import { localManifest, packageLocalBundle, publishLocalBundle, runOwnedProcess, targetTriple, watchBuildLease } from "./build-agent-bundle.mts";

async function directory(context: TestContext): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "ctl-local-bundle-"));
  context.after(() => rm(path, { recursive: true, force: true }));
  return path;
}

const files = Object.fromEntries(agentComponents.map((name) => [name, { sha256: "a".repeat(64), executable: true }]));

function elfFixture() {
  const word = (value: number): Buffer => {
    const bytes = Buffer.alloc(2);
    bytes.writeUInt16LE(value);
    return bytes;
  };
  const text = (value: string): Buffer => Buffer.concat([word(Buffer.byteLength(value)), Buffer.from(value)]);
  const target = "aarch64-unknown-linux-musl";
  const component = {
    build: { version: "0.1.2", source_revision: "a".repeat(40), source_fingerprint: "b".repeat(64), dirty: true },
    protocols: [{ name: "test", build: 2, version: "1.1.2", supported_versions: ["1.0.1", "1.1.2"] }],
  };
  const payload = Buffer.concat([
    text("ctmux"), text(target), text(component.build.version), text(component.build.source_revision), text(component.build.source_fingerprint),
    Buffer.from([1]), word(1),
    text("test"), word(2), word(1), word(1), word(2), word(2),
    word(1), word(0), word(1), word(1), word(1), word(2),
  ]);
  const size = Buffer.alloc(4);
  size.writeUInt32LE(payload.length);
  const record = Buffer.concat([Buffer.from("\0tokn-ctl-component\0\x01"), size, payload]);
  const header = Buffer.alloc(120);
  header.set([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1]);
  header.writeUInt16LE(3, 16); // Static PIE.
  header.writeUInt16LE(183, 18);
  header.writeBigUInt64LE(64n, 32);
  header.writeUInt16LE(56, 54);
  header.writeUInt16LE(1, 56);
  header.writeUInt32LE(1, 64); // Read-only PT_LOAD.
  header.writeUInt32LE(4, 68);
  header.writeBigUInt64LE(BigInt(header.length), 72);
  header.writeBigUInt64LE(BigInt(record.length), 96);
  return { header, record, target, component } as const;
}

test("ELF packaging reads the linked component and independently checks executable identity and architecture", () => {
  const { header, record, target, component } = elfFixture();
  const binary = Buffer.concat([header, record]);
  assert.deepEqual(readElfComponent(binary, target, "ctmux"), component);
  assert.throws(() => readElfComponent(binary, target, "ctmuxd"), /different executable or target/);
  assert.throws(() => readElfComponent(binary, targetTriple, "ctmux"), /expected a Linux ELF executable/);
});

test("ELF packaging rejects a dynamic interpreter, detached metadata and duplicate records", () => {
  const { header, record, target } = elfFixture();
  const binary = () => Buffer.concat([header, record]);
  header.writeUInt32LE(3, 64); // PT_INTERP.
  assert.throws(() => readElfComponent(binary(), target, "ctmux"), /dynamic interpreter/);
  header.writeUInt32LE(1, 64);
  header.writeBigUInt64LE(0n, 96);
  assert.throws(() => readElfComponent(binary(), target, "ctmux"), /no linked component metadata/);
  header.writeBigUInt64LE(BigInt(record.length * 2), 96);
  assert.throws(() => readElfComponent(Buffer.concat([header, record, record]), target, "ctmux"), /exactly one embedded component record/);
});

test("local bundles retain dirty source identity and use canonical Rust manifest field order", () => {
  const components = componentMapFixture();
  for (const component of Object.values(components)) component.build.dirty = true;
  const manifest = localManifest(components, files);
  assert.equal(manifest.source, "local");
  assert.equal(manifest.target_triple, targetTriple);
  assert.equal(manifest.components.ctmuxd.build.dirty, true);
  assert.equal(manifest.schema_version, 2);
  assert.deepEqual(Object.keys(manifest.components), ["ctl", "ctl-agent", "ctl-taskd", "ctld", "ctmux", "ctmuxd"]);
  // This digest is also checked against the real Rust reader in the local build verification.
  assert.equal(manifest.bundle_id, createHash("sha256").update(JSON.stringify({ ...manifest, bundle_id: "" })).digest("hex"));
  const reordered = Object.fromEntries(Object.entries(components).reverse().map(([name, component]) => [name, {
    protocols: component.protocols, build: { dirty: true, source_fingerprint: component.build.source_fingerprint,
      source_revision: component.build.source_revision, version: component.build.version },
  }]));
  assert.deepEqual(localManifest(reordered, files), manifest);
});

test("a local bundle rejects components from different working trees or without a common contract", () => {
  const components = componentMapFixture();
  components.ctmuxd.build.source_fingerprint = "b".repeat(64);
  assert.throws(() => localManifest(components, files), /different working trees/);
  const incompatible = componentMapFixture();
  incompatible.ctmuxd.protocols = incompatible.ctmuxd.protocols.filter((entry) => entry.name !== "ctmux");
  assert.throws(() => localManifest(incompatible, files), /required protocol/);
});

test("packaging snapshots the services and both CLIs and queries the copied target binaries", async (context) => {
  const root = await directory(context);
  const binaries = join(root, "binaries");
  const output = join(root, "bundle");
  await mkdir(binaries);
  const components = componentMapFixture();
  for (const name of agentComponents) {
    components[name].build.dirty = true;
    await writeFile(join(binaries, name), `Linux executable ${name}`);
    await chmod(join(binaries, name), 0o755);
  }
  const inspected: string[] = [];
  const manifest = await packageLocalBundle(binaries, output, async (program, args) => {
    assert.deepEqual(args, ["--component-info"]);
    assert.equal(program, join(output, basename(program)));
    inspected.push(basename(program));
    return JSON.stringify(components[basename(program) as keyof typeof components]);
  });
  assert.deepEqual(inspected, [...agentComponents]);
  assert.deepEqual((await readdir(output)).sort(), [...agentComponents, "bundle.json"].sort());
  if (process.platform !== "win32") {
    assert.equal((await lstat(output)).mode & 0o777, 0o700);
    assert.equal((await lstat(join(output, "bundle.json"))).mode & 0o777, 0o600);
    for (const name of agentComponents) assert.equal((await lstat(join(output, name))).mode & 0o777, 0o700);
  }
  await writeFile(join(binaries, "ctmuxd"), "later build");
  assert.equal((await readFile(join(output, "ctmuxd"))).toString(), "Linux executable ctmuxd");
  assert.equal(manifest.files.ctmuxd.sha256, createHash("sha256").update("Linux executable ctmuxd").digest("hex"));
});

test("failed target inspection never writes a complete bundle manifest", async (context) => {
  const root = await directory(context);
  await writeFile(join(root, "ctl-agent"), "executable");
  const output = join(root, "output");
  await assert.rejects(packageLocalBundle(root, output, async () => { throw new Error("cannot execute Linux binary"); }), /cannot execute Linux binary/);
  await assert.rejects(readFile(join(output, "bundle.json")), { code: "ENOENT" });
});

test("publishing restores previous output when the new staging directory cannot be moved", async (context) => {
  const root = await directory(context);
  const destination = join(root, "bundle");
  await mkdir(destination);
  await writeFile(join(destination, "bundle.json"), "previous");
  await assert.rejects(publishLocalBundle(join(root, "absent"), destination), { code: "ENOENT" });
  assert.equal(await readFile(join(destination, "bundle.json"), "utf8"), "previous");
});

test("a stopped build lease cancels the owned worker; advancing leases keep it alive", async () => {
  let counter = 0;
  let now = 0;
  let reads = 0;
  let cancelled = false;
  const stop = watchBuildLease(async () => { reads += 1; return String(counter); }, () => { cancelled = true; }, 100, 5, () => now);
  const tick = async (): Promise<void> => {
    const before = reads;
    for (let attempt = 0; attempt < 100 && reads === before; attempt += 1) await delay(10);
    assert.ok(reads > before);
  };
  try {
    await tick();
    now = 1_000; counter += 1;
    await tick();
    assert.equal(cancelled, false);
    now += 101;
    for (let attempt = 0; attempt < 40 && !cancelled; attempt += 1) await delay(10);
    assert.equal(cancelled, true);
  } finally { stop(); }
});

test("cancelling the launcher reaps its owned engine and compiler child", async (context) => {
  const root = await directory(context);
  const pidfile = join(root, "engine.pid");
  const abort = new AbortController();
  const engine = "const worker = require('node:child_process').spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {stdio: 'ignore'}); require('node:fs').writeFileSync(process.argv[1], JSON.stringify([process.pid, worker.pid])); setInterval(() => {}, 1000)";
  const running = runOwnedProcess(process.execPath, ["-e", engine, pidfile], abort.signal);
  let pids: number[] = [];
  try {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      try { pids = JSON.parse(await readFile(pidfile, "utf8")); break; } catch { await delay(10); }
    }
    assert.equal(pids.length, 2, "engine and compiler child started");
    abort.abort();
    await assert.rejects(running, /cancelled/);
    for (const pid of pids) {
      let stopped = false;
      for (let attempt = 0; attempt < 200; attempt += 1) {
        try { process.kill(pid, 0); } catch (error) { if ((error as NodeJS.ErrnoException).code === "ESRCH") { stopped = true; break; } }
        await delay(10);
      }
      assert.equal(stopped, true, `owned process ${pid} stopped`);
    }
  } finally {
    abort.abort(); await running.catch(() => {});
    for (const pid of pids) { try { process.kill(pid, "SIGKILL"); } catch {} }
  }
});

test("abrupt launcher death disconnects the supervisor and stops its engine", async (context) => {
  const root = await directory(context);
  const pidfile = join(root, "engine.pid");
  const module = new URL("./build-agent-bundle.mts", import.meta.url).href;
  const engine = "require('node:fs').writeFileSync(process.argv[1], String(process.pid)); setInterval(() => {}, 1000)";
  const driver = spawn(process.execPath, ["--experimental-strip-types", "--disable-warning=ExperimentalWarning", "--input-type=module", "-e",
    `import {runOwnedProcess} from ${JSON.stringify(module)}; await runOwnedProcess(process.execPath, ${JSON.stringify(["-e", engine, pidfile])}, new AbortController().signal);`,
  ], { stdio: "ignore" });
  const closed = once(driver, "close");
  let pid: number | undefined;
  try {
    for (let attempt = 0; attempt < 200; attempt += 1) {
      try { pid = Number(await readFile(pidfile, "utf8")); break; } catch { await delay(10); }
    }
    assert.ok(pid, "owned engine started");
    driver.kill("SIGKILL");
    await closed;
    let stopped = false;
    for (let attempt = 0; attempt < 200; attempt += 1) {
      try { process.kill(pid, 0); } catch (error) { if ((error as NodeJS.ErrnoException).code === "ESRCH") { stopped = true; break; } }
      await delay(10);
    }
    assert.equal(stopped, true, "engine stopped after parent IPC disconnected");
  } finally {
    driver.kill("SIGKILL");
    if (pid) { try { process.kill(pid, "SIGKILL"); } catch {} }
    await closed;
  }
});
