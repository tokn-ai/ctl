import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import { agentComponents, type AgentBundleIdentity, type AgentComponentMap } from "../shared/agent-bundle.mts";
import type { ComponentInfo } from "../shared/protocol-contract.mts";

export const agentIdentity: AgentBundleIdentity = {
  app_version: "0.1.0", bundle_id: "0.1.0-dev.0123456789ab", git_revision: "0123456789abcdef0123456789abcdef01234567",
};

export function componentFixture(names: string[], identity = agentIdentity): ComponentInfo {
  const builds: Record<string, number> = { ctl_identity: 3, ctl_maintenance: 2, ctmux: 13, ctmux_control: 1, task: 4, task_control: 2 };
  return { build: { version: identity.app_version, source_revision: identity.git_revision, source_fingerprint: "a".repeat(64), dirty: false },
    protocols: names.map((name) => ({ name, build: builds[name], version: `1.0.${builds[name]}`, supported_versions: [`1.0.${builds[name]}`] })) };
}

export function componentMapFixture(identity = agentIdentity): AgentComponentMap {
  return { "ctl-agent": componentFixture(["ctl_identity", "ctl_maintenance", "ctmux", "ctmux_control", "task", "task_control"], identity),
    ctmuxd: componentFixture(["ctmux", "ctmux_control"], identity),
    "ctl-taskd": componentFixture(["task", "task_control", "ctmux", "ctmux_control"], identity) };
}

export interface TarFixtureEntry {
  name: string;
  bytes: Buffer;
  type?: number;
  declared_size?: number;
  mode?: number;
}

export function tarFixture(entries: TarFixtureEntry[], terminator = true): Buffer {
  const parts: Buffer[] = [];
  for (const entry of entries) {
    const header = Buffer.alloc(512);
    header.write(entry.name, 0, 100, "utf8");
    const octal = (offset: number, length: number, value: number): void => { header.write(`${value.toString(8).padStart(length - 1, "0")}\0`, offset, length, "ascii"); };
    octal(100, 8, entry.mode ?? 0o755);
    octal(108, 8, 0); octal(116, 8, 0);
    octal(124, 12, entry.declared_size ?? entry.bytes.length); octal(136, 12, 0);
    header.fill(32, 148, 156);
    header[156] = entry.type ?? 48;
    header.write("ustar\0", 257, "ascii"); header.write("00", 263, "ascii");
    const checksum = header.reduce((sum, byte) => sum + byte, 0);
    header.write(`${checksum.toString(8).padStart(6, "0")}\0 `, 148, 8, "ascii");
    parts.push(header, entry.bytes, Buffer.alloc((512 - entry.bytes.length % 512) % 512));
  }
  if (terminator) parts.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(parts));
}

export function archiveEntriesFixture(target: string, identity = agentIdentity, components = componentMapFixture(identity)): TarFixtureEntry[] {
  const entries: TarFixtureEntry[] = agentComponents.map((name) => ({ name, bytes: Buffer.from(`fixture ${name} ${target}`) }));
  const files = Object.fromEntries(entries.map((entry) => [entry.name, createHash("sha256").update(entry.bytes).digest("hex")]));
  entries.push({ name: "manifest.json", bytes: Buffer.from(JSON.stringify({ schema_version: 2, ...identity, target_triple: target, files, components })) });
  return entries;
}
