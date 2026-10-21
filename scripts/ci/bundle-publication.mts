import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { join } from "node:path";
import { agentTargets, maxAgentManifestBytes, parseAgentBundleSet, readAgentFile, verifyAgentBundleTarget,
  type AgentBundleIdentity, type AgentBundleSet } from "../shared/agent-bundle.mts";

export const bundleRoot = "https://tokn-ai.dev/ctl/bundles/";
export type BundleChannel = "dev" | "stable";

export function componentIdentity(value: AgentBundleIdentity): AgentBundleIdentity {
  return { app_version: value.app_version, bundle_id: value.bundle_id, git_revision: value.git_revision };
}

export async function validatedComponentSet(directory: string, identity: AgentBundleIdentity): Promise<{ manifest: AgentBundleSet; asset_names: string[] }> {
  const expected = componentIdentity(identity);
  const manifest = parseAgentBundleSet(await readAgentFile(join(directory, "bundle-set.json"), maxAgentManifestBytes), expected);
  if (manifest.schema_version !== 2) throw new Error("Published component bundles must use schema 2");
  const asset_names = ["bundle-set.json"];
  for (const target of agentTargets) {
    const entry = manifest.targets[target];
    await verifyAgentBundleTarget(directory, expected, target, entry);
    asset_names.push(entry.archive, `${entry.archive}.sha256`);
  }
  return { manifest, asset_names };
}

export async function fileDigest(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return `sha256:${hash.digest("hex")}`;
}

export function repositoryName(value: string): string {
  if (!/^[a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+$/.test(value)) throw new Error("Repository must be OWNER/REPO");
  return value;
}
