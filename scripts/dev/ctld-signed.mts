import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseHelperComponent, sameProtocols, type ComponentInfo, type ProtocolInfo } from "../shared/protocol-contract.mts";
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
  env?: NodeJS.ProcessEnv;
  home_directory?: string;
  renewal?: { expected_team: string; expected_component: ComponentInfo };
}

/** Sign a prebuilt native helper without compiling, selecting or installing it. */
export async function signDevelopmentHelper(
  options: DevelopmentSigningOptions, run: MacosCommandRunner = runMacosCommand,
): Promise<{ app: string; manifest: DevelopmentHelperManifest }> {
  const architecture = architectures.get(options.target);
  if (!architecture) throw new Error("signed development builds require a native macOS Rust target");
  const renewal = options.renewal;
  if (renewal && !/^[A-Z0-9]{10}$/.test(renewal.expected_team)) throw new Error("invalid installed helper Apple Team ID");
  const env = developmentEnvironment(options.env ?? process.env);
  const invoke = (command: string, args: string[], extra_env: NodeJS.ProcessEnv = {}, stream_output = false, timeout_ms?: number) =>
    run(command, args, { cwd: options.repository_root, env: { ...env, ...extra_env }, stream_output, timeout_ms });
  const artifact = join(options.output_directory, ".ctld-input");
  const identity_path = join(options.output_directory, ".signing-identity");
  const payload = join(options.output_directory, ".helper-payload");
  const profile = await prepareProvisioningProfile({
    repository_root: options.repository_root, target_directory: options.target_directory,
    env, run, home_directory: options.home_directory, expected_team: renewal?.expected_team,
  });
  try {
    await ensurePrivateDirectory(options.output_directory);
  } catch (error) {
    await profile.close?.();
    throw error;
  }
  try {
    await regularFile(options.artifact);
    await copyFile(options.artifact, artifact);
    await chmod(artifact, 0o700);
    // Renewal trusts metadata from the verified installation until the new
    // signature is in place. macOS may refuse to run the expired executable.
    const component = renewal?.expected_component ??
      parseHelperComponent((await invoke(artifact, ["--component-info"], {}, false, 10_000)).stdout);
    const build = component.build;
    if (build.version !== options.app_version || typeof build.source_revision !== "string" || !/^[a-f0-9]{40}$/.test(build.source_revision)) {
      throw new Error("locally compiled ctld has invalid source identity or version");
    }
    const app = join(options.output_directory, "ctld.app");
    console.error("Signing ctld with your provisioning profile…");
    await invoke("/bin/sh", ["scripts/ci/package-ctld-app.sh", artifact, app, options.app_version], {
      CTLD_PROVISIONING_PROFILE: profile.path, CTLD_REQUIRE_DISTRIBUTION_SIGNING: "false",
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
    if (renewal && team !== renewal.expected_team) throw new Error("renewed ctld must use the installed helper's Apple Team");
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
    try {
      await rm(artifact, { force: true });
      await rm(identity_path, { force: true });
      await rm(payload, { recursive: true, force: true });
    } finally {
      await profile.close?.();
    }
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

async function requirePrivateInput(path: string, maximum: number): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.size === 0 || info.size > maximum || (info.mode & 0o077) !== 0 ||
    (process.getuid && info.uid !== process.getuid())) throw new Error("invalid private ctld renewal input");
}

async function renewSigning(descriptor: string): Promise<void> {
  if (!isAbsolute(descriptor) || resolve(descriptor) !== descriptor) throw new Error("ctld renewal requires an absolute descriptor path");
  const staging = dirname(descriptor);
  await requirePrivateInput(descriptor, 64 * 1024);
  await ensurePrivateDirectory(staging);
  const value: unknown = JSON.parse(await readFile(descriptor, "utf8"));
  if (!value || typeof value !== "object" || Array.isArray(value) ||
    Object.keys(value).sort().join() !== "artifact,expected_component,expected_team,output_directory,target") {
    throw new Error("invalid ctld renewal descriptor");
  }
  const { artifact, expected_component, expected_team, output_directory, target } = value as Record<string, unknown>;
  if (typeof artifact !== "string" || dirname(artifact) !== staging || resolve(artifact) !== artifact || artifact === descriptor ||
    typeof output_directory !== "string" || dirname(output_directory) !== staging || resolve(output_directory) !== output_directory ||
    typeof target !== "string" || !architectures.has(target) || typeof expected_team !== "string" || !/^[A-Z0-9]{10}$/.test(expected_team)) {
    throw new Error("invalid ctld renewal descriptor");
  }
  await requirePrivateInput(artifact, 128 * 1024 * 1024);
  const component = parseHelperComponent(JSON.stringify(expected_component));
  await mkdir(output_directory, { mode: 0o700 });
  await signDevelopmentHelper({
    repository_root, target_directory: staging, artifact, target, app_version: component.build.version,
    output_directory, renewal: { expected_component: component, expected_team },
  });
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log("usage: pnpm ctld:provision | pnpm ctld:build");
    console.log("  ctld:provision  open the shared Xcode provisioning project");
    console.log("  ctld:build      prepare the complete native development installation through ctlup");
    return;
  }
  if (args[0] === "--renew-signing" && args.length === 2) {
    if (process.platform !== "darwin") throw new Error("signed helper development is only available on macOS");
    const monitor = process.env.CTLUP_DEVELOPMENT_LIFELINE === "1";
    const canceled = () => process.kill(-process.pid, "SIGKILL");
    if (monitor) {
      process.stdin.once("end", canceled);
      process.stdin.once("error", canceled);
      process.stdin.resume();
    }
    try {
      await renewSigning(args[1]!);
    } finally {
      if (monitor) {
        process.stdin.off("end", canceled);
        process.stdin.off("error", canceled);
        process.stdin.destroy();
      }
    }
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
