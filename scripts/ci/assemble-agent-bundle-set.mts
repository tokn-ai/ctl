import { createHash } from "node:crypto";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { agentTargets, archiveName, inspectAgentArchive, maxAgentManifestBytes, parseAgentBundleSet, readAgentFile, validateAgentIdentity,
  type AgentBundleIdentity, type AgentBundleSet } from "../shared/agent-bundle.mts";

/** Copy only complete, verified component sets, without executing foreign binaries. */
export async function assembleAgentBundleSet(identity: AgentBundleIdentity, input_directory: string, output_directory: string): Promise<AgentBundleSet> {
  validateAgentIdentity(identity);
  const targets: AgentBundleSet["targets"] = {};
  for (const target of agentTargets) {
    const archive = archiveName(identity, target);
    const path = join(input_directory, archive);
    const bytes = await readAgentFile(path, 128 * 1024 * 1024);
    const sha256 = createHash("sha256").update(bytes).digest("hex");
    const sidecar = (await readAgentFile(`${path}.sha256`, 256)).toString("utf8").trim().split(/\s+/u);
    if (sidecar.length !== 2 || sidecar[0].toLowerCase() !== sha256 || sidecar[1] !== archive) throw new Error(`checksum mismatch or invalid sidecar for ${archive}`);
    const manifest = await inspectAgentArchive(bytes, identity, target);
    targets[target] = { archive, sha256, components: manifest.components };
  }
  const manifest: AgentBundleSet = { schema_version: 2, ...identity, targets };
  const manifestBytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
  parseAgentBundleSet(manifestBytes, identity);
  if (manifestBytes.length > maxAgentManifestBytes) throw new Error("agent bundle set exceeds its size limit");
  await mkdir(output_directory, { recursive: true });
  for (const target of agentTargets) {
    const archive = targets[target].archive;
    await copyFile(join(input_directory, archive), join(output_directory, archive));
    await copyFile(join(input_directory, `${archive}.sha256`), join(output_directory, `${archive}.sha256`));
  }
  await writeFile(join(output_directory, "bundle-set.json"), manifestBytes);
  return manifest;
}

async function main(): Promise<void> {
  const [app_version, bundle_id, git_revision, input_directory, output_directory, ...extra] = process.argv.slice(2);
  if (!output_directory || extra.length) throw new Error("usage: assemble-agent-bundle-set.sh APP_VERSION BUNDLE_ID GIT_REVISION INPUT_DIRECTORY OUTPUT_DIRECTORY");
  await assembleAgentBundleSet({ app_version, bundle_id, git_revision }, input_directory, output_directory);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(`agent bundle assembly failed: ${error instanceof Error ? error.message : String(error)}`); process.exitCode = 1; });
}
