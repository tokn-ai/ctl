import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, lstat, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { agentComponents, agentTargets, archiveName, inspectAgentArchive, parseAgentComponents, readAgentFile, validateAgentIdentity,
  type AgentBundleIdentity, type AgentBundleManifest, type AgentComponentMap } from "../shared/agent-bundle.mts";
import { maxComponentMetadata } from "../shared/protocol-contract.mts";

const execute = promisify(execFile);
export type AgentProcessRunner = (command: string, args: string[]) => Promise<{ stdout: string; stderr: string }>;
export interface AgentBundleOptions extends AgentBundleIdentity {
  target: string;
  output_directory: string;
  binary_directory: string;
}

export async function packageAgentBundle(options: AgentBundleOptions, run: AgentProcessRunner = (command, args) => execute(command, args, {
  timeout: args[0] === "--component-info" ? 5_000 : undefined,
  maxBuffer: args[0] === "--component-info" ? maxComponentMetadata : 1024 * 1024,
  env: { ...process.env, COPYFILE_DISABLE: "1" },
})): Promise<AgentBundleManifest> {
  validateAgentIdentity(options);
  if (!(agentTargets as readonly string[]).includes(options.target)) throw new Error(`unsupported agent bundle target: ${options.target}`);
  const output = resolve(options.output_directory);
  await mkdir(output, { recursive: true });
  const staging = await mkdtemp(join(output, ".package-"));
  try {
    for (const name of agentComponents) {
      const path = join(options.binary_directory, name);
      const info = await lstat(path);
      if (!info.isFile() || info.size === 0 || (info.mode & 0o111) === 0) throw new Error(`missing release executable: ${path}`);
      await copyFile(path, join(staging, name));
    }
    try {
      await run("strip", agentComponents.map((name) => join(staging, name)));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    const components = {} as AgentComponentMap;
    const files = {} as AgentBundleManifest["files"];
    for (const name of agentComponents) {
      const binary = join(staging, name);
      // Execute the packaged bytes on their native runner (or macOS Rosetta).
      // Metadata must never be invented from a different host-target build.
      const { stdout } = await run(binary, ["--component-info"]);
      if (Buffer.byteLength(stdout) > maxComponentMetadata) throw new Error(`${name} reported oversized component metadata`);
      components[name] = JSON.parse(stdout);
      files[name] = createHash("sha256").update(await readAgentFile(binary, 128 * 1024 * 1024)).digest("hex");
    }
    const { app_version, bundle_id, git_revision, target } = options;
    const identity = { app_version, bundle_id, git_revision };
    const manifest: AgentBundleManifest = { schema_version: 2, ...identity, target_triple: target, files,
      components: parseAgentComponents(components, identity) };
    await writeFile(join(staging, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    const archive = archiveName(identity, target);
    const archivePath = join(staging, archive);
    await run("tar", ["--format=ustar", "-czf", archivePath, "-C", staging, ...agentComponents, "manifest.json"]);
    const bytes = await readAgentFile(archivePath, 128 * 1024 * 1024);
    await inspectAgentArchive(bytes, identity, target);
    await copyFile(archivePath, join(output, archive));
    await writeFile(join(output, `${archive}.sha256`), `${createHash("sha256").update(bytes).digest("hex")}  ${archive}\n`);
    return manifest;
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const [target, app_version, bundle_id, git_revision, output_directory, ...extra] = process.argv.slice(2);
  if (!output_directory || extra.length) throw new Error("usage: package-agent-bundle.sh TARGET APP_VERSION BUNDLE_ID GIT_REVISION OUTPUT_DIRECTORY");
  const binary_directory = join(process.env.CARGO_TARGET_DIR ?? "target", target, "release");
  await packageAgentBundle({ target, app_version, bundle_id, git_revision, output_directory, binary_directory });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(`agent bundle packaging failed: ${error instanceof Error ? error.message : String(error)}`); process.exitCode = 1; });
}
