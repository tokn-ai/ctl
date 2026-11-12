import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { appendFile, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";
import { windowsAgentTarget, type AgentBundleIdentity } from "../shared/agent-bundle.mts";
import { parseComponent } from "../shared/protocol-contract.mts";
import { updaterBinaryName, updaterManifestName } from "../shared/updater-bundle.mts";
import { assembleWindowsAgentBundleSet } from "./assemble-agent-bundle-set.mts";
import { packageAgentBundle } from "./package-agent-bundle.mts";
import { packageUpdater } from "./package-updater.mts";

const execute = promisify(execFile);

interface Status {
  active: { distribution_id: string; target_triple: string } | null;
  previous: { distribution_id: string } | null;
}

interface HostCatalog {
  document: { hosts: { name: string; remote_info?: { remote_id: string; agent_version: string } }[] };
}

export async function json(program: string, args: string[], options: { timeout_ms?: number } = {}): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const child = spawn(program, args, { stdio: ["ignore", "pipe", "pipe"] });
    const stdout: Buffer[] = [];
    let stdout_bytes = 0;
    let stderr = Buffer.alloc(0);
    let failure: Error | undefined;
    const timer = setTimeout(() => {
      failure ??= new Error("Command timed out");
      child.kill();
      child.stdout.destroy();
      child.stderr.destroy();
    }, options.timeout_ms ?? 120_000);
    child.stdout.on("data", (chunk: Buffer) => {
      stdout_bytes += chunk.length;
      if (stdout_bytes > 1024 * 1024) {
        failure ??= new Error("Command JSON exceeded the 1 MiB output limit");
        child.kill();
        child.stdout.destroy();
        child.stderr.destroy();
      } else stdout.push(chunk);
    });
    child.stderr.on("data", (chunk: Buffer) => {
      // Progress can exceed a megabyte during a large verified upload. Drain it
      // continuously while retaining the tail needed to explain a failure.
      stderr = Buffer.concat([stderr, chunk]).subarray(-32 * 1024);
    });
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      if (failure || code !== 0) {
        const reason = failure?.message ?? `Command exited ${signal ?? code}`;
        reject(new Error(`${reason}: ${program}\n${stderr.toString("utf8").trim()}`));
        return;
      }
      try { resolve(JSON.parse(Buffer.concat(stdout).toString("utf8"))); }
      catch (error) { reject(error); }
    });
  });
}

async function status(program: string, target: string[]): Promise<Status> {
  return await json(program, [...target, "status", "--json"]) as Status;
}

async function bootstrapFiles(home: string): Promise<string[]> {
  return (await readdir(home)).filter((name) => /^ctlup-[0-9a-f]{32}\.exe$/i.test(name)).sort();
}

async function main(): Promise<void> {
  if (process.platform !== "win32" || process.env.GITHUB_ACTIONS !== "true") throw new Error("Run this fixture on an ephemeral GitHub Windows runner");
  const [binaries, packages, destination, saved_host, ...extra] = process.argv.slice(2);
  if (!binaries || !packages || !!destination !== !!saved_host || extra.length) {
    throw new Error("usage: windows-ctlup-smoke.mts BINARY_DIRECTORY PACKAGE_DIRECTORY|- [SSH_DESTINATION SAVED_HOST]");
  }
  const binary_directory = resolve(binaries);
  const manager = join(binary_directory, "ctlup.exe");
  const build = parseComponent(await json(manager, ["--component-info"])).build;
  assert.ok(build.source_revision);
  const base_identity = { app_version: build.version, git_revision: build.source_revision };
  const target = destination ? ["--destination", destination, "--remote-platform", "windows"] : [];
  const home = homedir();
  const root = join(home, ".tokn", "ctl");
  const managed_entries = ["bin", "installs", "leases", "updaters", ".activation.json", "installations.lock"];
  for (const entry of managed_entries) {
    const existing = await lstat(join(root, entry)).catch((error: NodeJS.ErrnoException) => {
      if (error.code !== "ENOENT") throw error;
      return undefined;
    });
    assert.equal(existing, undefined, `Refusing to replace existing managed state: ${entry}`);
  }
  const fixture = await mkdtemp(join(tmpdir(), "ctlup smoke "));
  let catalog_restore: { path: string; contents: Buffer } | undefined;
  try {
    const preview = `${build.version}-dev.${build.source_revision.slice(0, 12)}`;
    const first_packages = packages === "-" ? join(fixture, "first packages") : resolve(packages);
    const first_id = packages === "-" ? preview : JSON.parse(await readFile(join(first_packages, updaterManifestName(windowsAgentTarget)), "utf8")).bundle_id;
    const first: AgentBundleIdentity = { ...base_identity, bundle_id: first_id };
    if (packages === "-") {
      const first_options = { ...first, target: windowsAgentTarget, binary_directory, output_directory: first_packages };
      await packageAgentBundle(first_options);
      await packageUpdater(first_options);
    }
    const second: AgentBundleIdentity = { ...base_identity, bundle_id: first.bundle_id === preview ? build.version : preview };
    const second_packages = join(fixture, "second packages");
    const second_options = { ...second, target: windowsAgentTarget, binary_directory, output_directory: second_packages };
    await packageAgentBundle(second_options);
    // Give the fixture updater a new digest without changing its executable
    // sections, so the update exercises the running manager's replacement.
    const alternate_directory = join(fixture, "alternate updater");
    await mkdir(alternate_directory);
    const alternate_manager = join(alternate_directory, "ctlup.exe");
    await copyFile(manager, alternate_manager);
    await appendFile(alternate_manager, "ctlup smoke test\n");
    const alternate = await packageUpdater({ ...second_options, binary_directory: alternate_directory });
    const first_bundle = join(fixture, "first bundle");
    const second_bundle = join(fixture, "second bundle");
    await assembleWindowsAgentBundleSet(first, first_packages, first_bundle);
    await assembleWindowsAgentBundleSet(second, second_packages, second_bundle);
    const bootstrap = join(first_bundle, updaterBinaryName(first, windowsAgentTarget));
    const installed = join(root, "bin", "ctlup.exe");
    const initial_digest = createHash("sha256").update(await readFile(bootstrap)).digest("hex");
    assert.notEqual(alternate.sha256, initial_digest);
    const assert_digest = async (expected: string): Promise<void> => {
      const { stdout } = await execute(installed, ["updater-digest"], { timeout: 30_000 });
      assert.equal(stdout.trim(), expected);
    };
    const before_bootstrap = await bootstrapFiles(home);
    const list_sessions = [...target, "ctmux", "list"];

    assert.deepEqual(await status(bootstrap, target), { active: null, previous: null });
    if (destination) await assert.rejects(execute(join(binary_directory, "ctl.exe"), list_sessions, { timeout: 30_000 }));
    await json(bootstrap, [...target, "install", "--from", first_bundle, "--json"]);
    await assert_digest(initial_digest);
    assert.deepEqual(await bootstrapFiles(home), before_bootstrap);
    const initial = await status(installed, target);
    assert.equal(initial.active?.distribution_id, first.bundle_id);
    assert.equal(initial.active?.target_triple, windowsAgentTarget);
    assert.equal(initial.previous, null);
    const ctl = join(root, "bin", "ctl.exe");
    assert.equal(parseComponent(await json(ctl, ["--component-info"])).build.source_revision, build.source_revision);
    let pinned_target = target;
    let set_pin: ((remote_id: string) => Promise<void>) | undefined;
    let account_id: string | undefined;
    if (destination) {
      await execute(ctl, list_sessions, { timeout: 30_000 });
      const path = process.env.CTL_HOSTS_PATH;
      assert.ok(path);
      const contents = await readFile(path);
      catalog_restore = { path, contents };
      const catalog = JSON.parse(contents.toString("utf8")) as HostCatalog;
      const host = catalog.document.hosts.find((entry) => entry.name === saved_host);
      assert.ok(host);
      account_id = await readFile(join(root, "remote-id"), "ascii");
      set_pin = async (remote_id: string) => {
        host.remote_info = { remote_id, agent_version: build.version };
        await writeFile(path, JSON.stringify(catalog));
      };
      await set_pin(account_id);
      pinned_target = ["--host", saved_host];
    }

    const update = [...pinned_target, "update", "--from", second_bundle, "--json"];
    const rollback = [...pinned_target, "rollback", "--json"];
    await json(installed, update);
    await assert_digest(alternate.sha256);
    const updated = await status(installed, pinned_target);
    assert.deepEqual(await bootstrapFiles(home), before_bootstrap);
    assert.equal(updated.active?.distribution_id, second.bundle_id);
    assert.equal(updated.previous?.distribution_id, first.bundle_id);
    if (set_pin && account_id) {
      await set_pin(randomUUID());
      await assert.rejects(json(installed, rollback), /SSH account identity changed/i);
      assert.deepEqual(await status(installed, target), updated);
      await set_pin(account_id);
    }
    await json(installed, rollback);
    const rolled_back = await status(installed, pinned_target);
    assert.equal(rolled_back.active?.distribution_id, first.bundle_id);
    assert.equal(rolled_back.previous?.distribution_id, second.bundle_id);
    await assert_digest(alternate.sha256);
    if (destination) {
      // Re-select the cached installation after rollback with the same updater.
      await json(installed, update);
      assert.deepEqual(await status(installed, pinned_target), updated);
      assert.deepEqual(await bootstrapFiles(home), before_bootstrap);
    }
    console.log(`Windows ctlup install, update and rollback passed${destination ? " over OpenSSH" : ""}.`);
  } finally {
    if (catalog_restore) await writeFile(catalog_restore.path, catalog_restore.contents);
    for (const path of [fixture, ...managed_entries.map((entry) => join(root, entry))]) {
      await rm(path, { recursive: true, force: true, maxRetries: 3 });
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => {
    console.error(error);
    process.exitCode = 1;
  });
}
