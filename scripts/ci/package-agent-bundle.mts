import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, lstat, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { addAgentPayloadBytes, agentComponents, archiveName, executableName, inspectAgentArchive, isAgentTarget, parseAgentComponents, readAgentFile, validateAgentIdentity, windowsAgentTarget,
  type AgentBundleIdentity, type AgentBundleManifest, type AgentComponentMap } from "../shared/agent-bundle.mts";
import { maxComponentMetadata } from "../shared/protocol-contract.mts";
import { probeBundledComponent } from "../shared/component-probe.mts";

const execute = promisify(execFile);
export type AgentProcessRunner = (command: string, args: string[], cwd?: string) => Promise<{ stdout: string; stderr: string }>;
export interface AgentBundleOptions extends AgentBundleIdentity {
  target: string;
  output_directory: string;
  binary_directory: string;
}

export async function packageAgentBundle(options: AgentBundleOptions, run: AgentProcessRunner = (command, args, cwd) => {
  const env = { ...process.env, COPYFILE_DISABLE: "1" };
  return args[0] === "--component-info"
    ? probeBundledComponent(command, args, { env, target: options.target })
    : execute(command, args, { cwd, maxBuffer: 1024 * 1024, env });
}): Promise<AgentBundleManifest> {
  validateAgentIdentity(options);
  if (!isAgentTarget(options.target)) throw new Error(`unsupported agent bundle target: ${options.target}`);
  const output = resolve(options.output_directory);
  await mkdir(output, { recursive: true });
  const staging = await mkdtemp(join(output, ".package-"));
  const windows = options.target === windowsAgentTarget;
  const executableNames = agentComponents.map((name) => executableName(name, options.target));
  try {
    for (const name of executableNames) {
      const path = join(options.binary_directory, name);
      const info = await lstat(path);
      if (!info.isFile() || info.size === 0 || (!windows && (info.mode & 0o111) === 0)) throw new Error(`missing release executable: ${path}`);
      await copyFile(path, join(staging, name));
    }
    if (!windows) {
      try {
        const stripped = await run("strip", executableNames.map((name) => join(staging, name)));
        if (stripped.stderr) process.stderr.write(`strip: ${stripped.stderr.slice(0, 2048)}\n`);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      }
    }
    const components = {} as AgentComponentMap;
    const files = {} as AgentBundleManifest["files"];
    let payloadBytes = 0;
    for (const name of agentComponents) {
      const binary = join(staging, executableName(name, options.target));
      // Execute the packaged bytes on their native runner (or macOS Rosetta).
      // Metadata must never be invented from a different host-target build.
      const { stdout } = await run(binary, ["--component-info"]);
      if (Buffer.byteLength(stdout) > maxComponentMetadata) throw new Error(`${name} reported oversized component metadata`);
      components[name] = JSON.parse(stdout);
      const bytes = await readAgentFile(binary, 128 * 1024 * 1024);
      payloadBytes = addAgentPayloadBytes(payloadBytes, bytes.length);
      files[name] = createHash("sha256").update(bytes).digest("hex");
    }
    const { app_version, bundle_id, git_revision, target } = options;
    const identity = { app_version, bundle_id, git_revision };
    const manifest: AgentBundleManifest = { schema_version: 3, ...identity, target_triple: target, files,
      components: parseAgentComponents(components, identity) };
    const manifestBytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
    addAgentPayloadBytes(payloadBytes, manifestBytes.length);
    await writeFile(join(staging, "manifest.json"), manifestBytes);
    const archive = archiveName(identity, target);
    const archivePath = join(staging, archive);
    await run("tar", ["--format=ustar", "-czf", archive, ...executableNames, "manifest.json"], staging);
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
  if (!output_directory || extra.length) throw new Error("usage: package-agent-bundle.mts TARGET APP_VERSION BUNDLE_ID GIT_REVISION OUTPUT_DIRECTORY");
  const binary_directory = join(process.env.CARGO_TARGET_DIR ?? "target", target, "release");
  await packageAgentBundle({ target, app_version, bundle_id, git_revision, output_directory, binary_directory });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(`agent bundle packaging failed: ${error instanceof Error ? error.message : String(error)}`); process.exitCode = 1; });
}
