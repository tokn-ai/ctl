import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { readAgentFile, validateAgentIdentity, type AgentBundleIdentity } from "../shared/agent-bundle.mts";
import { probeBundledComponent } from "../shared/component-probe.mts";
import { parseComponent } from "../shared/protocol-contract.mts";
import { maxUpdaterBytes, parseUpdaterManifest, updaterBinaryName, updaterManifestName, verifyUpdaterTarget, type UpdaterManifest } from "../shared/updater-bundle.mts";

export interface PackageUpdaterOptions extends AgentBundleIdentity {
  target: string;
  binary_directory: string;
  output_directory: string;
  allow_dirty?: boolean;
  strip?: boolean;
}

export async function packageUpdater(options: PackageUpdaterOptions,
  probe = (path: string) => probeBundledComponent(path, ["--component-info"], { target: options.target })): Promise<UpdaterManifest> {
  validateAgentIdentity(options);
  const input = join(options.binary_directory, "ctlup");
  await mkdir(options.output_directory, { recursive: true });
  const stage = await mkdtemp(join(options.output_directory, ".ctlup-"));
  try {
    await readAgentFile(input, maxUpdaterBytes);
    const executable = join(stage, "ctlup");
    await copyFile(input, executable);
    await chmod(executable, 0o700);
    if (options.strip !== false) await promisify(execFile)("strip", [executable], { maxBuffer: 1024 * 1024 });
    const bytes = await readAgentFile(executable, maxUpdaterBytes);
    const { stdout } = await probe(executable);
    const info = parseComponent(JSON.parse(stdout));
    if (info.build.version !== options.app_version || info.build.source_revision !== options.git_revision || (!options.allow_dirty && info.build.dirty)) {
      throw new Error("ctlup was not built from this clean publication source");
    }
    const manifest: UpdaterManifest = { schema_version: 1, app_version: options.app_version, bundle_id: options.bundle_id,
      git_revision: options.git_revision, target_triple: options.target, binary: updaterBinaryName(options, options.target),
      sha256: createHash("sha256").update(bytes).digest("hex"), size: bytes.length };
    const metadata = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
    parseUpdaterManifest(metadata, options, options.target);
    const output = join(options.output_directory, manifest.binary);
    await copyFile(executable, output);
    await chmod(output, 0o700);
    await writeFile(`${output}.sha256`, `${manifest.sha256}  ${manifest.binary}\n`);
    await writeFile(join(options.output_directory, updaterManifestName(options.target)), metadata);
    await verifyUpdaterTarget(options.output_directory, options, options.target, true);
    return manifest;
  } finally {
    await rm(stage, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const [target, app_version, bundle_id, git_revision, output_directory, ...extra] = process.argv.slice(2);
  if (!output_directory || extra.length) throw new Error("usage: package-updater.mts TARGET APP_VERSION BUNDLE_ID GIT_REVISION OUTPUT_DIRECTORY");
  await packageUpdater({ target, app_version, bundle_id, git_revision, output_directory,
    binary_directory: join(process.env.CARGO_TARGET_DIR ?? "target", target, "release") });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
