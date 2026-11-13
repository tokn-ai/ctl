import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { join } from "node:path";
import { agentTargets, maxAgentManifestBytes, parseAgentBundleSet, readAgentFile, readWindowsAgentBundleSet, verifyAgentBundleTarget, windowsAgentManifestName, windowsAgentTarget,
  type AgentBundleIdentity, type AgentBundleSet } from "../shared/agent-bundle.mts";
import { verifyUpdaterTarget } from "../shared/updater-bundle.mts";
import { buildInfoName, readBuildInfo } from "../shared/build-info.mts";

export const bundleRoot = "https://tokn-ai.dev/ctl/bundles/";
export type BundleChannel = "nightly" | "stable";

export function componentIdentity(value: AgentBundleIdentity): AgentBundleIdentity {
  return { app_version: value.app_version, bundle_id: value.bundle_id, git_revision: value.git_revision };
}

export async function validatedComponentSet(directory: string, identity: AgentBundleIdentity): Promise<{ manifest: AgentBundleSet; asset_names: string[] }> {
  const expected = componentIdentity(identity);
  const manifest = parseAgentBundleSet(await readAgentFile(join(directory, "bundle-set.json"), maxAgentManifestBytes), expected);
  if (manifest.schema_version === 1) throw new Error("Published component bundles require component metadata");
  const asset_names = ["bundle-set.json"];
  if (await readBuildInfo(directory, expected)) asset_names.push(buildInfoName);
  let updater_assets = 0;
  for (const target of agentTargets) {
    const entry = manifest.targets[target];
    await verifyAgentBundleTarget(directory, expected, target, entry);
    asset_names.push(entry.archive, `${entry.archive}.sha256`);
    const updater = await verifyUpdaterTarget(directory, expected, target);
    updater_assets += updater.length;
    asset_names.push(...updater);
  }
  if (updater_assets !== 0 && updater_assets !== agentTargets.length * 3) throw new Error("component set contains an incomplete ctlup target set");
  asset_names.push(...await validatedWindowsComponentSet(directory, expected));
  return { manifest, asset_names };
}

export async function validatedWindowsComponentSet(directory: string, identity: AgentBundleIdentity): Promise<string[]> {
  const manifest = await readWindowsAgentBundleSet(directory, componentIdentity(identity));
  if (!manifest) return [];
  const entry = manifest.targets[windowsAgentTarget];
  await verifyAgentBundleTarget(directory, identity, windowsAgentTarget, entry);
  const updater = await verifyUpdaterTarget(directory, identity, windowsAgentTarget, true);
  return [windowsAgentManifestName, entry.archive, `${entry.archive}.sha256`, ...updater];
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
