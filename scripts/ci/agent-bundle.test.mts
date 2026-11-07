import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { gunzipSync, gzipSync } from "node:zlib";
import { addAgentPayloadBytes, agentComponents, agentTargets, archiveName, inspectAgentArchive, parseAgentBundleSet, parseAgentComponents, readAgentFile, verifyAgentBundleTarget } from "../shared/agent-bundle.mts";
import { agentIdentity, archiveEntriesFixture, componentMapFixture, tarFixture } from "./agent-bundle.fixtures.mts";
import { assembleAgentBundleSet } from "./assemble-agent-bundle-set.mts";
import { packageAgentBundle, type AgentProcessRunner } from "./package-agent-bundle.mts";

const execute = promisify(execFile);
const target = agentTargets[0];

test("archive file sizes include the inner manifest in the 512 MiB managed import limit", () => {
  const mib = 1024 * 1024;
  const binary_sizes = [100, 100, 100, 100, 100, 12].map((size) => size * mib);
  const manifest_size = 4096;
  binary_sizes[0] -= manifest_size;
  const file_sizes = [...binary_sizes, manifest_size];
  assert.equal(file_sizes.reduce(addAgentPayloadBytes, 0), 512 * mib);
  assert.throws(() => [...binary_sizes, manifest_size + 1].reduce(addAgentPayloadBytes, 0), /total payload size limit/);
});

async function directory(t: TestContext): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "ctl-agent-package-test-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}

test("packages actual executable metadata and assembles all targets without executing archive members", async (t) => {
  const root = await directory(t);
  const binaries = join(root, "release"); const input = join(root, "input"); const output = join(root, "output");
  await mkdir(binaries);
  const components = componentMapFixture();
  for (const name of agentComponents) {
    const path = join(binaries, name);
    await writeFile(path, `#!/bin/sh\nif [ "$1" != "--component-info" ]; then exit 1; fi\ncat <<'COMPONENT'\n${JSON.stringify(components[name])}\nCOMPONENT\n`);
    await chmod(path, 0o755);
  }
  const inspected = new Set<string>();
  const runner: AgentProcessRunner = async (command, args) => {
    if (command === "strip") return { stdout: "", stderr: "" };
    if (args[0] === "--component-info") inspected.add(basename(command));
    return execute(command, args, { env: { ...process.env, COPYFILE_DISABLE: "1" } });
  };
  for (const target of agentTargets) {
    const manifest = await packageAgentBundle({ ...agentIdentity, target, binary_directory: binaries, output_directory: input }, runner);
    assert.deepEqual(manifest.components, components);
  }
  assert.deepEqual([...inspected].sort(), [...agentComponents].sort());
  const set = await assembleAgentBundleSet(agentIdentity, input, output);
  assert.equal(set.schema_version, 3);
  const parsed = parseAgentBundleSet(await readFile(join(output, "bundle-set.json")), agentIdentity);
  for (const target of agentTargets) {
    // Development sync passes the complete catalog, which extends the build identity.
    assert.deepEqual((await verifyAgentBundleTarget(output, parsed, target, parsed.targets[target]))?.components, components);
  }
  assert.ok((await readdir(input)).every((name) => !name.startsWith(".package-")));
});

test("schema-2 archives and sets retain exactly the original four services", async () => {
  const legacy = archiveEntriesFixture(target, agentIdentity, componentMapFixture(), 2);
  const archive = await inspectAgentArchive(tarFixture(legacy), agentIdentity, target);
  assert.equal(archive.schema_version, 2);
  assert.deepEqual(Object.keys(archive.components).sort(), ["ctl-agent", "ctl-taskd", "ctld", "ctmuxd"]);
  const targets = Object.fromEntries(agentTargets.map((target) => [target, {
    archive: archiveName(agentIdentity, target), sha256: "a".repeat(64), components: archive.components,
  }]));
  const set = { ...agentIdentity, schema_version: 2, targets };
  assert.equal(parseAgentBundleSet(Buffer.from(JSON.stringify(set))).schema_version, 2);
  assert.throws(() => parseAgentBundleSet(Buffer.from(JSON.stringify({ ...set, schema_version: 3 }))));
  targets[target].components = componentMapFixture();
  assert.throws(() => parseAgentBundleSet(Buffer.from(JSON.stringify(set))));
  assert.throws(() => parseAgentBundleSet(Buffer.from(JSON.stringify({ ...set, schema_version: 1 }))));

  const cli = { name: "ctl", bytes: Buffer.from("unexpected CLI in schema 2") };
  await assert.rejects(inspectAgentArchive(tarFixture([cli, ...legacy]), agentIdentity, target));
  const newer = archiveEntriesFixture(target).filter((entry) => entry.name !== "ctmux");
  await assert.rejects(inspectAgentArchive(tarFixture(newer), agentIdentity, target));
});

test("metadata rejects mixed source builds, incomplete maps and invented or incoherent contracts", async (t) => {
  const cases: [string, (value: ReturnType<typeof componentMapFixture>) => void][] = [
    ["dirty build", (value) => { value.ctmuxd.build.dirty = true; }],
    ["other revision", (value) => { value.ctmuxd.build.source_revision = "b".repeat(40); }],
    ["other release", (value) => { value["ctl-taskd"].build.version = "0.2.0"; }],
    ["missing remote control daemon", (value) => { Reflect.deleteProperty(value, "ctld"); }],
    ["control daemon source mismatch", (value) => { value.ctld.build.source_revision = "b".repeat(40); }],
    ["missing control helper contract", (value) => { value.ctld.protocols.pop(); }],
    ["missing remote VPN contract", (value) => { value["ctl-agent"].protocols = value["ctl-agent"].protocols.filter((protocol) => protocol.name !== "ctl_remote_vpn"); }],
    ["missing companion contract", (value) => { value["ctl-agent"].protocols.pop(); }],
    ["incomplete CLI pair", (value) => { Reflect.deleteProperty(value, "ctl"); }],
    ["ctmux client missing control", (value) => { value.ctmux.protocols.pop(); }],
    ["ctl client mismatches remote identity", (value) => { value.ctl.protocols[0] = { name: "ctl_identity", build: 4, version: "2.0.4", supported_versions: ["2.0.4"] }; }],
    ["duplicate contract", (value) => { value.ctmuxd.protocols.push(value.ctmuxd.protocols[0]); }],
    ["latest omitted", (value) => { value.ctmuxd.protocols[0].supported_versions = ["1.0.12"]; }],
    ["invented range", (value) => { value.ctmuxd.protocols[0].supported_versions.push("1.1.12"); }],
    ["companion mismatch", (value) => { value.ctmuxd.protocols[0] = { name: "ctmux", build: 14, version: "2.0.14", supported_versions: ["2.0.14"] }; }],
    ["task consumer mismatch", (value) => { value["ctl-taskd"].protocols[2] = { name: "ctmux", build: 14, version: "2.0.14", supported_versions: ["2.0.14"] }; }],
    ["control daemon mismatch", (value) => { value.ctld.protocols[0] = { name: "ctld", build: 14, version: "2.0.14", supported_versions: ["2.0.14"] }; }],
  ];
  for (const [name, mutate] of cases) await t.test(name, () => {
    const components = componentMapFixture(); mutate(components);
    assert.throws(() => parseAgentComponents(components, agentIdentity));
  });
  const newer = componentMapFixture();
  for (const component of Object.values(newer)) {
    const protocol = component.protocols.find((item) => item.name === "ctmux");
    if (!protocol) continue;
    protocol.build = 15; protocol.version = "1.1.15"; protocol.supported_versions.push("1.1.15");
  }
  assert.deepEqual(parseAgentComponents(newer, agentIdentity), newer);
});

test("shared service transport advertisements retain legacy bundles and require matching peers", () => {
  const components = componentMapFixture();
  const transport = { name: "ctl_transport", build: 1, version: "1.0.1", supported_versions: ["1.0.1"] };
  assert.deepEqual(parseAgentComponents(components, agentIdentity), components);
  components["ctl-agent"].protocols.push(transport);
  assert.deepEqual(parseAgentComponents(components, agentIdentity), components);
  components.ctl.protocols.push({ ...transport });
  components.ctld.protocols.push({ ...transport });
  assert.deepEqual(parseAgentComponents(components, agentIdentity), components);
  components.ctl.protocols[components.ctl.protocols.length - 1] = {
    name: "ctl_transport", build: 2, version: "2.0.2", supported_versions: ["2.0.2"],
  };
  assert.throws(() => parseAgentComponents(components, agentIdentity), /cannot share ctl_transport/);
});

test("archive inspection rejects corrupted, mixed and unsafe payloads even with a valid outer checksum", async (t) => {
  const cases: [string, (entries: ReturnType<typeof archiveEntriesFixture>) => void][] = [
    ["duplicate member", (entries) => { entries.push(entries[0]); }],
    ["extra member", (entries) => { entries.push({ name: "unexpected", bytes: Buffer.from("extra") }); }],
    ["traversal", (entries) => { entries[0].name = "../ctl-agent"; }],
    ["symlink", (entries) => { entries[0].type = 50; }],
    ["missing member", (entries) => { entries.splice(1, 1); }],
    ["missing control daemon", (entries) => { entries.splice(entries.findIndex((entry) => entry.name === "ctld"), 1); }],
    ["binary checksum", (entries) => { entries[1].bytes = Buffer.from("replaced binary"); }],
    ["control daemon checksum", (entries) => { entries.find((entry) => entry.name === "ctld")!.bytes = Buffer.from("replaced control daemon"); }],
    ["non executable", (entries) => { entries[1].mode = 0o644; }],
    ["oversized binary", (entries) => { entries[1].declared_size = 128 * 1024 * 1024 + 1; }],
    ["oversized manifest", (entries) => { entries.find((entry) => entry.name === "manifest.json")!.bytes = Buffer.alloc(64 * 1024 + 1, 32); }],
    ["wrong target", (entries) => {
      const entry = entries.find((entry) => entry.name === "manifest.json")!;
      const manifest = JSON.parse(entry.bytes.toString()); manifest.target_triple = agentTargets[1];
      entry.bytes = Buffer.from(JSON.stringify(manifest));
    }],
  ];
  for (const [name, mutate] of cases) await t.test(name, async () => {
    const entries = archiveEntriesFixture(target); mutate(entries);
    await assert.rejects(inspectAgentArchive(tarFixture(entries), agentIdentity, target));
  });
  const valid = tarFixture(archiveEntriesFixture(target));
  await assert.rejects(inspectAgentArchive(valid.subarray(0, valid.length - 4), agentIdentity, target));
  await assert.rejects(inspectAgentArchive(tarFixture(archiveEntriesFixture(target), false), agentIdentity, target));
  const corruptedHeader = gunzipSync(valid); corruptedHeader[0] ^= 1;
  await assert.rejects(inspectAgentArchive(gzipSync(corruptedHeader), agentIdentity, target));
});

test("outer metadata must match the checksummed archive's complete metadata", async (t) => {
  const root = await directory(t);
  const bytes = tarFixture(archiveEntriesFixture(target)); const archive = archiveName(agentIdentity, target);
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  await writeFile(join(root, archive), bytes);
  await writeFile(join(root, `${archive}.sha256`), `${sha256}  ${archive}\n`);
  const components = componentMapFixture(); components.ctld.build.source_fingerprint = "b".repeat(64);
  await assert.rejects(verifyAgentBundleTarget(root, agentIdentity, target, { archive, sha256, components }), /metadata differs/);
});

test("a corrupt target prevents assembly publication", async (t) => {
  const root = await directory(t); const output = join(root, "output");
  for (const target of agentTargets) {
    const archive = archiveName(agentIdentity, target); const bytes = tarFixture(archiveEntriesFixture(target));
    await writeFile(join(root, archive), bytes);
    await writeFile(join(root, `${archive}.sha256`), `${target === agentTargets[3] ? "0".repeat(64) : createHash("sha256").update(bytes).digest("hex")}  ${archive}\n`);
  }
  await assert.rejects(assembleAgentBundleSet(agentIdentity, root, output), /checksum mismatch/);
  await assert.rejects(readFile(join(output, "bundle-set.json")), { code: "ENOENT" });
});

test("special-file inputs fail promptly instead of blocking before validation", { skip: process.platform === "win32", timeout: 2_000 }, async (t) => {
  const root = await directory(t); const fifo = join(root, "bundle-set.json");
  await execute("mkfifo", [fifo]);
  await assert.rejects(readAgentFile(fifo, 64 * 1024), /invalid agent bundle file/);
});
