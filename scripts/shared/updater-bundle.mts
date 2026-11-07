import { createHash } from "node:crypto";
import { join } from "node:path";
import { agentTargets, readAgentFile, validateAgentIdentity, type AgentBundleIdentity } from "./agent-bundle.mts";

export const maxUpdaterBytes = 128 * 1024 * 1024;
export const maxUpdaterManifestBytes = 16 * 1024;

export interface UpdaterManifest extends AgentBundleIdentity {
  schema_version: 1;
  target_triple: string;
  binary: string;
  sha256: string;
  size: number;
}

export const updaterManifestName = (target: string): string => `ctlup-${target}.json`;
export const updaterBinaryName = (identity: AgentBundleIdentity, target: string): string => `ctlup-${identity.bundle_id}-${target}`;
export const updaterAssetNames = (manifest: UpdaterManifest): string[] => [manifest.binary, `${manifest.binary}.sha256`, updaterManifestName(manifest.target_triple)];

export function parseUpdaterManifest(bytes: Buffer, identity: AgentBundleIdentity, target: string): UpdaterManifest {
  validateAgentIdentity(identity);
  if (bytes.length > maxUpdaterManifestBytes) throw new Error("ctlup manifest exceeds its size limit");
  const value: unknown = JSON.parse(bytes.toString("utf8"));
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("invalid ctlup manifest");
  const manifest = value as UpdaterManifest;
  const fields = ["schema_version", "app_version", "bundle_id", "git_revision", "target_triple", "binary", "sha256", "size"];
  if (Object.keys(value).length !== fields.length || fields.some((name) => !Object.hasOwn(value, name)) ||
    manifest.schema_version !== 1 || manifest.app_version !== identity.app_version || manifest.bundle_id !== identity.bundle_id ||
    manifest.git_revision !== identity.git_revision || manifest.target_triple !== target ||
    !(agentTargets as readonly string[]).includes(target) || manifest.binary !== updaterBinaryName(identity, target) ||
    typeof manifest.sha256 !== "string" || !/^[a-f0-9]{64}$/.test(manifest.sha256) ||
    !Number.isSafeInteger(manifest.size) || manifest.size < 1 || manifest.size > maxUpdaterBytes) {
    throw new Error("ctlup manifest does not match the publication identity");
  }
  return manifest;
}

/** Legacy publications have no updater. Partial or corrupt updater sets fail closed. */
export async function verifyUpdaterTarget(directory: string, identity: AgentBundleIdentity, target: string, required = false): Promise<string[]> {
  let bytes: Buffer;
  try {
    bytes = await readAgentFile(join(directory, updaterManifestName(target)), maxUpdaterManifestBytes);
  } catch (error) {
    if (!required && (error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw error;
  }
  const manifest = parseUpdaterManifest(bytes, identity, target);
  const binary = await readAgentFile(join(directory, manifest.binary), maxUpdaterBytes);
  const checksum = (await readAgentFile(join(directory, `${manifest.binary}.sha256`), 512)).toString("utf8");
  if (binary.length !== manifest.size || createHash("sha256").update(binary).digest("hex") !== manifest.sha256 ||
    checksum !== `${manifest.sha256}  ${manifest.binary}\n`) throw new Error(`ctlup checksum mismatch for ${target}`);
  return updaterAssetNames(manifest);
}
