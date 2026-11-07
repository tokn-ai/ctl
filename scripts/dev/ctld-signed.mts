import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseHelperComponent, sameProtocols, type ProtocolInfo } from "../shared/protocol-contract.mts";
import {
  getCargoTargetDirectory, openProvisioningProject, prepareProvisioningProfile,
  runMacosCommand, type MacosCommandRunner,
} from "./macos-provisioning.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

const repository_root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const architectures = new Map([
  ["aarch64-apple-darwin", "arm64"], ["x86_64-apple-darwin", "x86_64"],
]);
const helper_identifier = "dev.tokn-ai.ctl.ctld";

export interface DevelopmentHelperManifest {
  schema_version: 1;
  component: "ctld";
  app_version: string;
  bundle_id: string;
  git_revision: string;
  target: string;
  bundle_identifier: typeof helper_identifier;
  team_identifier: string;
  signing_mode: "development";
  notarized: false;
  archive: string;
  sha256: string;
  archive_size: number;
  development: { source_fingerprint: string; dirty: boolean };
  protocols: ProtocolInfo[];
}


export interface DevelopmentSigningOptions {
  repository_root: string;
  target_directory: string;
  artifact: string;
  target: string;
  app_version: string;
  output_directory: string;
  profile_path?: string;
  env?: NodeJS.ProcessEnv;
  home_directory?: string;
}

/** Sign a prebuilt native helper without compiling, selecting or installing it. */
export async function signDevelopmentHelper(
  options: DevelopmentSigningOptions, run: MacosCommandRunner = runMacosCommand,
): Promise<{ app: string; manifest: DevelopmentHelperManifest }> {
  const architecture = architectures.get(options.target);
  if (!architecture) throw new Error("signed development builds require a native macOS Rust target");
  const env = developmentEnvironment(options.env ?? process.env);
  const invoke = (command: string, args: string[], extra_env: NodeJS.ProcessEnv = {}, stream_output = false, timeout_ms?: number) =>
    run(command, args, { cwd: options.repository_root, env: { ...env, ...extra_env }, stream_output, timeout_ms });
  const profile = options.profile_path ?? (await prepareProvisioningProfile({
    repository_root: options.repository_root, target_directory: options.target_directory,
    env, run, home_directory: options.home_directory,
  })).path;
  await ensurePrivateDirectory(options.output_directory);
  const artifact = join(options.output_directory, ".ctld-input");
  const identity_path = join(options.output_directory, ".signing-identity");
  const payload = join(options.output_directory, ".helper-payload");
  try {
    await regularFile(options.artifact);
    await copyFile(options.artifact, artifact);
    await chmod(artifact, 0o700);
    const component = parseHelperComponent((await invoke(artifact, ["--component-info"], {}, false, 10_000)).stdout);
    const build = component.build;
    if (build.version !== options.app_version || typeof build.source_revision !== "string" || !/^[a-f0-9]{40}$/.test(build.source_revision)) {
      throw new Error("locally compiled ctld has invalid source identity or version");
    }
    const app = join(options.output_directory, "ctld.app");
    console.error("Signing ctld with your provisioning profile…");
    await invoke("/bin/sh", ["scripts/ci/package-ctld-app.sh", artifact, app, options.app_version], {
      CTLD_PROVISIONING_PROFILE: profile, CTLD_REQUIRE_DISTRIBUTION_SIGNING: "false",
      CTLD_SIGNING_IDENTITY_OUTPUT: identity_path, CTLD_SIGNING_TIMESTAMP: "none",
    }, true);
    for (const relative of ["Contents/MacOS/ctld", "Contents/Info.plist", "Contents/embedded.provisionprofile", "Contents/_CodeSignature/CodeResources"]) {
      await regularFile(join(app, relative));
    }
    const executable = join(app, "Contents/MacOS/ctld");
    await invoke("lipo", [executable, "-verify_arch", architecture]);
    await invoke("codesign", ["--verify", "--strict", app]);
    const team = /^TeamIdentifier=([A-Z0-9]{10})$/m.exec((await invoke("codesign", ["-d", "--verbose=2", app])).stderr)?.[1];
    if (!team) throw new Error("signed ctld does not report a valid Apple Team ID");
    await regularFile(identity_path);
    const identity = (await readFile(identity_path, "utf8")).trim();
    if (!/^[A-Fa-f0-9]{40}$/.test(identity)) throw new Error("invalid matching signing certificate fingerprint");
    const signed_component = parseHelperComponent((await invoke(executable, ["--component-info"], {}, false, 10_000)).stdout);
    if (signed_component.build.version !== build.version || signed_component.build.source_revision !== build.source_revision ||
      signed_component.build.source_fingerprint !== build.source_fingerprint || signed_component.build.dirty !== build.dirty ||
      !sameProtocols(signed_component.protocols, component.protocols)) throw new Error("signed helper differs from the prebuilt component");
    await mkdir(payload);
    const archive = `ctld-${options.app_version}-${options.target}.app.tar.gz`;
    await invoke("env", ["COPYFILE_DISABLE=1", "tar", "--format", "ustar", "-czf", join(payload, archive), "-C", options.output_directory, "ctld.app"]);
    await regularFile(join(payload, archive));
    const bytes = await readFile(join(payload, archive));
    if (bytes.length > 128 * 1024 * 1024) throw new Error("ctld development archive exceeds the install limit");
    const sha256 = createHash("sha256").update(bytes).digest("hex");
    const manifest: DevelopmentHelperManifest = {
      schema_version: 1, component: "ctld", app_version: options.app_version, bundle_id: `dev.${sha256}`,
      git_revision: build.source_revision, target: options.target, bundle_identifier: helper_identifier,
      team_identifier: team, signing_mode: "development", notarized: false,
      archive, sha256, archive_size: bytes.length,
      development: { source_fingerprint: build.source_fingerprint, dirty: build.dirty }, protocols: component.protocols,
    };
    const receipt = `${JSON.stringify(manifest, null, 2)}\n`;
    if (Buffer.byteLength(receipt) > 16 * 1024) throw new Error("ctld development receipt exceeds the install limit");
    await writeFile(join(options.output_directory, "installation.json"), receipt, { mode: 0o600 });
    return { app, manifest };
  } finally {
    await rm(artifact, { force: true });
    await rm(identity_path, { force: true });
    await rm(payload, { recursive: true, force: true });
  }
}

export function developmentEnvironment(environment: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  return {
    ...environment, CTL_BUNDLED_CTLD_DIR: undefined, CTL_BUNDLED_CTLD_MODE: undefined,
    CTLD_REQUIRE_DISTRIBUTION_SIGNING: undefined, CTLD_SIGNING_IDENTITY_OUTPUT: undefined,
    CTLD_SIGNING_TIMESTAMP: undefined,
    CTLD_ASKPASS: undefined, CTLD_ASKPASS_TOKEN: undefined, CTLD_IDENTITY_ASKPASS: undefined,
    CTLD_IDENTITY_ASKPASS_SOCKET: undefined, CTLD_IDENTITY_ASKPASS_TOKEN: undefined,
  };
}

async function regularFile(path: string): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.size === 0) throw new Error(`expected a nonempty regular file: ${path}`);
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log("usage: pnpm ctld:provision | pnpm ctld:build");
    console.log("  ctld:provision  open the shared Xcode provisioning project");
    console.log("  ctld:build      prepare the complete native development installation through ctlup");
    return;
  }
  if (args.length > 1 || (args.length === 1 && args[0] !== "--provision")) {
    throw new Error("usage: pnpm ctld:provision | pnpm ctld:build");
  }
  if (process.platform !== "darwin") throw new Error("signed helper development is only available on macOS");
  if (args[0] === "--provision") {
    await openProvisioningProject({ repository_root, target_directory: await getCargoTargetDirectory(repository_root) });
  } else {
    const { prepareDevelopment } = await import("./ctlup-development.mts");
    const prepared = await prepareDevelopment([], repository_root, process.env);
    await prepared.close();
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
