import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { readAgentFile, validateAgentIdentity, type AgentBundleIdentity } from "./agent-bundle.mts";

export const buildInfoName = "build-info.json";
export const maxBuildInfoBytes = 16 * 1024;

export interface BuildInfo extends AgentBundleIdentity {
  schema_version: 1;
  built_at: string;
}

export function parseBuildInfo(bytes: Buffer, identity: AgentBundleIdentity): BuildInfo {
  validateAgentIdentity(identity);
  if (bytes.length > maxBuildInfoBytes) throw new Error("Build metadata exceeds its size limit");
  const value = JSON.parse(bytes.toString("utf8")) as BuildInfo;
  if (!value || typeof value !== "object" || Array.isArray(value) ||
    Object.keys(value).length !== 5 || value.schema_version !== 1 ||
    value.app_version !== identity.app_version || value.bundle_id !== identity.bundle_id ||
    value.git_revision !== identity.git_revision || typeof value.built_at !== "string" ||
    !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/.test(value.built_at) ||
    !Number.isFinite(Date.parse(value.built_at)) ||
    new Date(value.built_at).toISOString().slice(0, 19) !== value.built_at.slice(0, 19)) {
    throw new Error("Build metadata does not match the publication identity or a UTC timestamp");
  }
  return value;
}

export async function readBuildInfo(directory: string, identity: AgentBundleIdentity): Promise<BuildInfo | undefined> {
  try {
    return parseBuildInfo(await readAgentFile(join(directory, buildInfoName), maxBuildInfoBytes), identity);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
}

export async function writeBuildInfo(directory: string, identity: AgentBundleIdentity): Promise<void> {
  if (await readBuildInfo(directory, identity)) return;
  const info: BuildInfo = { ...identity, schema_version: 1, built_at: new Date().toISOString() };
  await writeFile(join(directory, buildInfoName), `${JSON.stringify(info, null, 2)}\n`, { flag: "wx" });
}
