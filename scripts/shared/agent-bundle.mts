import { createHash, type Hash } from "node:crypto";
import { constants } from "node:fs";
import { open } from "node:fs/promises";
import { join } from "node:path";
import { Readable } from "node:stream";
import { createGunzip } from "node:zlib";
import { negotiateProtocol, parseComponent, sameProtocols, type ComponentInfo } from "./protocol-contract.mts";

export const agentTargets = [
  "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin", "aarch64-apple-darwin",
] as const;
export const agentComponents = ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"] as const;
export const maxAgentManifestBytes = 64 * 1024;
const maxArchiveBytes = 128 * 1024 * 1024;
const maxUnpackedBytes = agentComponents.length * maxArchiveBytes + maxAgentManifestBytes + 1024 * 1024;

export interface AgentBundleIdentity {
  app_version: string;
  bundle_id: string;
  git_revision: string;
}
export type AgentComponentMap = Record<typeof agentComponents[number], ComponentInfo>;
export interface AgentBundleManifest extends AgentBundleIdentity {
  schema_version: 2;
  target_triple: string;
  files: Record<typeof agentComponents[number], string>;
  components: AgentComponentMap;
}
export interface AgentBundleTarget {
  archive: string;
  sha256: string;
  components?: AgentComponentMap;
}
export interface AgentBundleSet extends AgentBundleIdentity {
  schema_version: 1 | 2;
  targets: Record<string, AgentBundleTarget>;
}

function record(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${name} must be a JSON object`);
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[], name: string): void {
  if (Object.keys(value).sort().join() !== [...keys].sort().join()) throw new Error(`${name} contains missing or unexpected fields`);
}

export function validateAgentIdentity(identity: AgentBundleIdentity): void {
  if (!/^[a-zA-Z0-9._+-]{1,128}$/.test(identity.app_version) || !/^[a-zA-Z0-9._+-]{1,128}$/.test(identity.bundle_id) ||
    !/^[a-fA-F0-9]{40}$/.test(identity.git_revision)) throw new Error("invalid agent bundle identity");
  if (identity.bundle_id !== identity.app_version && identity.bundle_id !== `${identity.app_version}-dev.${identity.git_revision.slice(0, 12)}`) {
    throw new Error("bundle id does not match its app version and Git revision");
  }
}

export function parseAgentComponents(value: unknown, identity: AgentBundleIdentity): AgentComponentMap {
  const entries = record(value, "bundle components");
  exactKeys(entries, agentComponents, "bundle components");
  const required = {
    "ctl-agent": ["ctl_identity", "ctl_maintenance", "ctl_remote_vpn", "ctld", "ctmux", "ctmux_control", "task", "task_control"],
    ctmuxd: ["ctmux", "ctmux_control"],
    "ctl-taskd": ["task", "task_control", "ctmux", "ctmux_control"],
    ctld: ["ctld", "ctld_lifecycle", "ctld_helper"],
  };
  const parsed = {} as AgentComponentMap;
  for (const name of agentComponents) {
    const component = parseComponent(entries[name]);
    if (component.build.dirty || component.build.version !== identity.app_version || component.build.source_revision !== identity.git_revision) {
      throw new Error(`${name} source identity does not match the clean bundle`);
    }
    if (required[name].some((protocol) => !component.protocols.some((entry) => entry.name === protocol))) {
      throw new Error(`${name} omits a required protocol`);
    }
    parsed[name] = component;
  }
  for (const [name, protocols] of [["ctmuxd", ["ctmux", "ctmux_control"]], ["ctl-taskd", ["task", "task_control"]], ["ctld", ["ctld"]]] as const) {
    for (const protocol of protocols) {
      const client = parsed["ctl-agent"].protocols.find((entry) => entry.name === protocol)!;
      const server = parsed[name].protocols.find((entry) => entry.name === protocol)!;
      if (!negotiateProtocol(client, server)) throw new Error(`bundle components have no common ${protocol} contract`);
    }
  }
  for (const protocol of ["ctmux", "ctmux_control"]) {
    const client = parsed["ctl-taskd"].protocols.find((entry) => entry.name === protocol)!;
    const server = parsed.ctmuxd.protocols.find((entry) => entry.name === protocol)!;
    if (!negotiateProtocol(client, server)) throw new Error(`task daemon has no common ${protocol} contract with ctmuxd`);
  }
  return parsed;
}

function identityFrom(root: Record<string, unknown>): AgentBundleIdentity {
  const identity = { app_version: root.app_version, bundle_id: root.bundle_id, git_revision: root.git_revision };
  if (Object.values(identity).some((value) => typeof value !== "string")) throw new Error("missing agent bundle identity");
  validateAgentIdentity(identity as AgentBundleIdentity);
  return identity as AgentBundleIdentity;
}

export function parseAgentBundleSet(bytes: Buffer, expected?: Partial<AgentBundleIdentity>): AgentBundleSet {
  if (bytes.length > maxAgentManifestBytes) throw new Error("agent bundle set exceeds its size limit");
  const root = record(JSON.parse(bytes.toString("utf8")), "bundle set");
  exactKeys(root, ["schema_version", "app_version", "bundle_id", "git_revision", "targets"], "bundle set");
  if (root.schema_version !== 1 && root.schema_version !== 2) throw new Error("unsupported agent bundle-set schema version");
  const identity = identityFrom(root);
  for (const [field, value] of Object.entries(expected ?? {})) {
    if (identity[field as keyof AgentBundleIdentity] !== value) throw new Error(`bundle set identity does not match expected ${field}`);
  }
  const targets = record(root.targets, "bundle-set targets");
  exactKeys(targets, agentTargets, "bundle-set targets");
  const parsed: AgentBundleSet["targets"] = {};
  for (const target of agentTargets) {
    const entry = record(targets[target], `bundle target ${target}`);
    exactKeys(entry, root.schema_version === 2 ? ["archive", "sha256", "components"] : ["archive", "sha256"], `bundle target ${target}`);
    if (entry.archive !== archiveName(identity, target) || typeof entry.sha256 !== "string" || !/^[a-fA-F0-9]{64}$/.test(entry.sha256)) {
      throw new Error(`invalid bundle metadata for ${target}`);
    }
    parsed[target] = { archive: entry.archive as string, sha256: entry.sha256.toLowerCase(),
      ...(root.schema_version === 2 ? { components: parseAgentComponents(entry.components, identity) } : {}) };
  }
  return { schema_version: root.schema_version, ...identity, targets: parsed };
}

export function archiveName(identity: AgentBundleIdentity, target: string): string {
  return `ctl-agent-bundle-${identity.bundle_id}-${target}.tar.gz`;
}

export function parseAgentBundleManifest(bytes: Buffer, identity: AgentBundleIdentity, target: string): AgentBundleManifest {
  if (bytes.length > maxAgentManifestBytes) throw new Error("agent bundle manifest exceeds its size limit");
  const root = record(JSON.parse(bytes.toString("utf8")), "archive manifest");
  exactKeys(root, ["schema_version", "app_version", "bundle_id", "git_revision", "target_triple", "files", "components"], "archive manifest");
  if (root.schema_version !== 2 || root.target_triple !== target ||
    root.app_version !== identity.app_version || root.bundle_id !== identity.bundle_id ||
    root.git_revision !== identity.git_revision) throw new Error("archive manifest does not match the bundle identity and target");
  const files = record(root.files, "archive file hashes");
  exactKeys(files, agentComponents, "archive file hashes");
  if (Object.values(files).some((value) => typeof value !== "string" || !/^[a-fA-F0-9]{64}$/.test(value))) throw new Error("invalid archive file checksum");
  return { schema_version: 2, ...identity, target_triple: target,
    files: Object.fromEntries(Object.entries(files).map(([name, digest]) => [name, (digest as string).toLowerCase()])) as AgentBundleManifest["files"],
    components: parseAgentComponents(root.components, identity) };
}

function octal(bytes: Buffer): number {
  const value = bytes.toString("ascii").replace(/\0.*$/su, "").trim();
  if (!/^[0-7]+$/.test(value)) throw new Error("invalid archive tar number");
  const number = Number.parseInt(value, 8);
  if (!Number.isSafeInteger(number)) throw new Error("archive tar number exceeds its limit");
  return number;
}

/** Inspect bytes without extracting files or executing foreign-target components. */
export async function inspectAgentArchive(bytes: Buffer, identity: AgentBundleIdentity, target: string): Promise<AgentBundleManifest> {
  if (bytes.length > maxArchiveBytes) throw new Error("agent archive exceeds its compressed size limit");
  const hashes = new Map<string, string>();
  const seen = new Set<string>();
  const manifestParts: Buffer[] = [];
  let pending = Buffer.alloc(0);
  let total = 0;
  let current: { name: string; remaining: number; padding: number; hash: Hash } | undefined;
  let padding = 0;
  let endBlocks = 0;
  for await (const chunk of Readable.from([bytes]).pipe(createGunzip())) {
    total += chunk.length;
    if (total > maxUnpackedBytes) throw new Error("agent archive exceeds its unpacked size limit");
    pending = Buffer.concat([pending, chunk]);
    while (pending.length) {
      if (current) {
        const count = Math.min(pending.length, current.remaining);
        const part = pending.subarray(0, count);
        current.hash.update(part);
        if (current.name === "manifest.json") manifestParts.push(Buffer.from(part));
        pending = pending.subarray(count);
        current.remaining -= count;
        if (current.remaining) break;
        hashes.set(current.name, current.hash.digest("hex"));
        padding = current.padding;
        current = undefined;
      } else if (padding) {
        const count = Math.min(padding, pending.length);
        if (pending.subarray(0, count).some((byte) => byte !== 0)) throw new Error("invalid archive tar padding");
        padding -= count;
        pending = pending.subarray(count);
      } else {
        if (pending.length < 512) break;
        const header = pending.subarray(0, 512);
        pending = pending.subarray(512);
        if (header.every((byte) => byte === 0)) { endBlocks += 1; continue; }
        if (endBlocks) throw new Error("archive contains entries after its terminator");
        const checksum = octal(header.subarray(148, 156));
        const actual = header.reduce((sum, byte, index) => sum + (index >= 148 && index < 156 ? 32 : byte), 0);
        const name = header.subarray(0, 100).toString("utf8").replace(/\0.*$/su, "");
        const size = octal(header.subarray(124, 136));
        if (checksum !== actual || (header[156] !== 0 && header[156] !== 48) || header.subarray(157, 257).some((byte) => byte !== 0) ||
          header.subarray(345, 500).some((byte) => byte !== 0) || ![...agentComponents, "manifest.json"].includes(name) || seen.has(name)) {
          throw new Error("archive contains invalid, duplicate, or unexpected entries");
        }
        if (size === 0 || size > (name === "manifest.json" ? maxAgentManifestBytes : maxArchiveBytes) ||
          (name !== "manifest.json" && (octal(header.subarray(100, 108)) & 0o111) === 0)) throw new Error("archive contains an invalid file size or executable mode");
        seen.add(name);
        current = { name, remaining: size, padding: (512 - size % 512) % 512, hash: createHash("sha256") };
      }
    }
  }
  if (current || padding || pending.length || endBlocks < 2 || seen.size !== agentComponents.length + 1) throw new Error("agent archive is truncated or incomplete");
  const manifest = parseAgentBundleManifest(Buffer.concat(manifestParts), identity, target);
  for (const name of agentComponents) {
    if (hashes.get(name) !== manifest.files[name]) throw new Error(`archive checksum mismatch for ${name}`);
  }
  return manifest;
}

export function sameAgentComponents(left: AgentComponentMap, right: AgentComponentMap): boolean {
  return agentComponents.every((name) => {
    const a = left[name]; const b = right[name];
    return a.build.version === b.build.version && a.build.source_revision === b.build.source_revision &&
      a.build.source_fingerprint === b.build.source_fingerprint && a.build.dirty === b.build.dirty && sameProtocols(a.protocols, b.protocols);
  });
}

export async function readAgentFile(path: string, maximum: number): Promise<Buffer> {
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const info = await file.stat();
    if (!info.isFile() || info.size === 0 || info.size > maximum) throw new Error(`invalid agent bundle file: ${path}`);
    const chunks: Buffer[] = [];
    let total = 0;
    while (true) {
      const chunk = Buffer.alloc(Math.min(64 * 1024, maximum + 1 - total));
      const { bytesRead } = await file.read(chunk);
      if (!bytesRead) break;
      total += bytesRead;
      if (total > maximum) throw new Error(`agent bundle file exceeds its size limit: ${path}`);
      chunks.push(chunk.subarray(0, bytesRead));
    }
    return Buffer.concat(chunks);
  } finally {
    await file.close();
  }
}

export async function verifyAgentBundleTarget(directory: string, identity: AgentBundleIdentity, target: string, entry: AgentBundleTarget): Promise<AgentBundleManifest | undefined> {
  const path = join(directory, entry.archive);
  const bytes = await readAgentFile(path, maxArchiveBytes);
  const actual = createHash("sha256").update(bytes).digest("hex");
  const sidecar = (await readAgentFile(`${path}.sha256`, 256)).toString("utf8").trim().split(/\s+/u);
  if (actual !== entry.sha256 || sidecar.length !== 2 || sidecar[0].toLowerCase() !== actual || sidecar[1] !== entry.archive) {
    throw new Error(`checksum mismatch or invalid checksum sidecar for ${entry.archive}`);
  }
  if (!entry.components) return undefined;
  const manifest = await inspectAgentArchive(bytes, identity, target);
  if (!sameAgentComponents(entry.components, manifest.components)) throw new Error("archive component metadata differs from bundle set");
  return manifest;
}
