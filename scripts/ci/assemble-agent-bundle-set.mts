import { createHash } from "node:crypto";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { agentTargets, archiveName, inspectAgentArchive, parseAgentBundleSet, parseWindowsAgentBundleSet, readAgentFile, validateAgentIdentity, windowsAgentManifestName, windowsAgentTarget,
  type AgentBundleIdentity, type AgentBundleSet } from "../shared/agent-bundle.mts";
import { verifyUpdaterTarget } from "../shared/updater-bundle.mts";

/** Copy only complete, verified component sets, without executing foreign binaries. */
export async function assembleAgentBundleSet(identity: AgentBundleIdentity, input_directory: string, output_directory: string): Promise<AgentBundleSet> {
  return assembleBundleSet(identity, input_directory, output_directory, false);
}

export async function assembleWindowsAgentBundleSet(identity: AgentBundleIdentity, input_directory: string, output_directory: string): Promise<AgentBundleSet> {
  return assembleBundleSet(identity, input_directory, output_directory, true);
}

async function assembleBundleSet(identity: AgentBundleIdentity, input_directory: string, output_directory: string, windows: boolean): Promise<AgentBundleSet> {
  validateAgentIdentity(identity);
  const targetTriples = windows ? [windowsAgentTarget] : agentTargets;
  const targets: AgentBundleSet["targets"] = {};
  const updater_assets: string[] = [];
  for (const target of targetTriples) {
    const archive = archiveName(identity, target);
    const path = join(input_directory, archive);
    const bytes = await readAgentFile(path, 128 * 1024 * 1024);
    const sha256 = createHash("sha256").update(bytes).digest("hex");
    const sidecar = (await readAgentFile(`${path}.sha256`, 256)).toString("utf8").trim().split(/\s+/u);
    if (sidecar.length !== 2 || sidecar[0].toLowerCase() !== sha256 || sidecar[1] !== archive) throw new Error(`checksum mismatch or invalid sidecar for ${archive}`);
    const manifest = await inspectAgentArchive(bytes, identity, target);
    if (manifest.schema_version !== 3) throw new Error(`New bundle sets require schema-3 archives: ${archive}`);
    targets[target] = { archive, sha256, components: manifest.components };
    updater_assets.push(...await verifyUpdaterTarget(input_directory, identity, target, windows));
  }
  if (updater_assets.length !== 0 && updater_assets.length !== targetTriples.length * 3) throw new Error("bundle set contains an incomplete ctlup target set");
  const manifest: AgentBundleSet = { ...identity, schema_version: 3, targets };
  const manifestBytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
  const parse = windows ? parseWindowsAgentBundleSet : parseAgentBundleSet;
  parse(manifestBytes, identity);
  await mkdir(output_directory, { recursive: true });
  for (const target of targetTriples) {
    const archive = targets[target].archive;
    await copyFile(join(input_directory, archive), join(output_directory, archive));
    await copyFile(join(input_directory, `${archive}.sha256`), join(output_directory, `${archive}.sha256`));
  }
  for (const name of updater_assets) await copyFile(join(input_directory, name), join(output_directory, name));
  await writeFile(join(output_directory, windows ? windowsAgentManifestName : "bundle-set.json"), manifestBytes);
  return manifest;
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  const windows = args[0] === "--windows";
  const [app_version, bundle_id, git_revision, input_directory, output_directory, ...extra] = windows ? args.slice(1) : args;
  if (!output_directory || extra.length) throw new Error("usage: assemble-agent-bundle-set.mts [--windows] APP_VERSION BUNDLE_ID GIT_REVISION INPUT_DIRECTORY OUTPUT_DIRECTORY");
  await assembleBundleSet({ app_version, bundle_id, git_revision }, input_directory, output_directory, windows);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(`agent bundle assembly failed: ${error instanceof Error ? error.message : String(error)}`); process.exitCode = 1; });
}
