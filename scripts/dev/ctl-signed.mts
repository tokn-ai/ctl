import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { binaryArtifact } from "../ci/build-ctl-bundle.mts";
import {
  getCargoTargetDirectory, openProvisioningProject, prepareProvisioningProfile,
  runMacosCommand, type MacosCommandRunner,
} from "./macos-provisioning.mts";
import { withPreparationLock } from "./signed-preparation-lock.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

const repository_root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const architectures = new Map([
  ["aarch64-apple-darwin", "arm64"], ["x86_64-apple-darwin", "x86_64"],
]);
const helper_identifier = "dev.tokn-ai.ctl.ctld";
const cli_identifier = "dev.tokn-ai.ctl.cli";

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
}

export interface DevelopmentBuildOptions {
  repository_root?: string;
  env?: NodeJS.ProcessEnv;
  home_directory?: string;
}

interface ComponentBuild {
  version: string;
  source_revision: string;
  source_fingerprint: string;
  dirty: boolean;
}

function componentBuild(stdout: string, version: string): ComponentBuild {
  if (Buffer.byteLength(stdout) > 16 * 1024) throw new Error("ctld reported oversized component metadata");
  const build = JSON.parse(stdout)?.build;
  if (!build || build.version !== version || typeof build.source_revision !== "string" ||
    !/^[a-f0-9]{40}$/.test(build.source_revision) || typeof build.source_fingerprint !== "string" ||
    !/^[a-f0-9]{64}$/.test(build.source_fingerprint) || typeof build.dirty !== "boolean") {
    throw new Error("locally compiled ctld has invalid source identity or version");
  }
  return build as ComponentBuild;
}

async function regularFile(path: string): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.size === 0) throw new Error(`expected a nonempty regular file: ${path}`);
}

/** Compile and locally sign a self-contained native CLI using Xcode provisioning. */
export async function buildSignedDevelopmentCli(
  options: DevelopmentBuildOptions = {}, run: MacosCommandRunner = runMacosCommand,
): Promise<string> {
  const root = resolve(options.repository_root ?? repository_root);
  const env: NodeJS.ProcessEnv = {
    ...(options.env ?? process.env), CTL_BUNDLED_CTLD_DIR: undefined, CTL_BUNDLED_CTLD_MODE: undefined,
    CTLD_REQUIRE_DISTRIBUTION_SIGNING: undefined, CTLD_SIGNING_IDENTITY_OUTPUT: undefined,
    CTLD_ASKPASS: undefined, CTLD_ASKPASS_TOKEN: undefined, CTLD_IDENTITY_ASKPASS: undefined,
    CTLD_IDENTITY_ASKPASS_SOCKET: undefined, CTLD_IDENTITY_ASKPASS_TOKEN: undefined,
  };
  const invoke = (command: string, args: string[], extra_env: NodeJS.ProcessEnv = {}, stream_output = false, timeout_ms?: number) =>
    run(command, args, {
      cwd: root, env: { ...env, ...extra_env }, stream_output, timeout_ms,
      stream_stderr: command === "cargo" && args[0] === "build",
    });
  const host = /^host: (\S+)$/m.exec((await invoke(env.RUSTC ?? env.CARGO_BUILD_RUSTC ?? "rustc", ["-vV"], {}, false, 10_000)).stdout)?.[1];
  const architecture = host && architectures.get(host);
  if (!host || !architecture) throw new Error("signed development CLI builds require a native macOS Rust target");
  const metadata = JSON.parse((await invoke("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"], {}, false, 30_000)).stdout);
  const helper_version = metadata.packages?.find((pkg: { name: string }) => pkg.name === "ctld")?.version;
  const cli_version = metadata.packages?.find((pkg: { name: string }) => pkg.name === "ctl-cli")?.version;
  if (typeof helper_version !== "string" || helper_version !== cli_version ||
    !/^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?(?:\+[a-zA-Z0-9.-]+)?$/.test(helper_version) ||
    typeof metadata.target_directory !== "string" || !isAbsolute(metadata.target_directory)) {
    throw new Error("Cargo must report matching ctl-cli/ctld versions and an absolute target directory");
  }
  const target_directory = metadata.target_directory as string;
  const profile = await prepareProvisioningProfile({
    repository_root: root, target_directory, env, run, home_directory: options.home_directory,
  });
  const output_directory = join(target_directory, "ctl-dev");
  await mkdir(output_directory, { recursive: true, mode: 0o700 });
  await ensurePrivateDirectory(output_directory);
  return withPreparationLock(output_directory, async () => {
    const temporary = await mkdtemp(join(output_directory, ".build-"));
    try {
      console.log("Building ctld…");
      // Keep embedded and ordinary Cargo builds in separate caches. The preparation
      // lock also covers signing and snapshotting for overlapping signed builds.
      const buildArgs = [
        "build", "--locked", "--target", host, "--target-dir", join(output_directory, "cargo"),
        "--message-format=json-render-diagnostics",
      ];
      const original = binaryArtifact((await invoke("cargo", [...buildArgs, "-p", "ctld"])).stdout, "ctld");
      const artifact = join(temporary, "ctld");
      await regularFile(original);
      // Cargo may replace its output during another build; inspect and sign this same snapshot.
      await copyFile(original, artifact);
      await chmod(artifact, 0o755);
      const build = componentBuild((await invoke(artifact, ["--component-info"], {}, false, 10_000)).stdout, helper_version);
      const app = join(temporary, "ctld.app");
      const identity_path = join(temporary, "signing-identity");
      console.log("Signing ctld with your provisioning profile…");
      await invoke("/bin/sh", ["scripts/ci/package-ctld-app.sh", artifact, app, helper_version], {
        CTLD_PROVISIONING_PROFILE: profile.path, CTLD_REQUIRE_DISTRIBUTION_SIGNING: "false",
        CTLD_SIGNING_IDENTITY_OUTPUT: identity_path,
      }, true);
      for (const relative of ["Contents/MacOS/ctld", "Contents/Info.plist", "Contents/embedded.provisionprofile", "Contents/_CodeSignature/CodeResources"]) {
        await regularFile(join(app, relative));
      }
      await invoke("lipo", ["-verify_arch", architecture, join(app, "Contents/MacOS/ctld")]);
      await invoke("codesign", ["--verify", "--strict", app]);
      const team = /^TeamIdentifier=([A-Z0-9]{10})$/m.exec((await invoke("codesign", ["-d", "--verbose=2", app])).stderr)?.[1];
      if (!team) throw new Error("signed ctld does not report a valid Apple Team ID");
      await regularFile(identity_path);
      const identity = (await readFile(identity_path, "utf8")).trim();
      if (!/^[A-Fa-f0-9]{40}$/.test(identity)) throw new Error("invalid matching signing certificate fingerprint");
      const payload = join(temporary, "payload");
      await mkdir(payload);
      const archive = `ctld-${helper_version}-${host}.app.tar.gz`;
      await invoke("env", ["COPYFILE_DISABLE=1", "tar", "--format", "ustar", "-czf", join(payload, archive), "-C", temporary, "ctld.app"]);
      await regularFile(join(payload, archive));
      const bytes = await readFile(join(payload, archive));
      if (bytes.length > 128 * 1024 * 1024) throw new Error("ctld development archive exceeds the install limit");
      const sha256 = createHash("sha256").update(bytes).digest("hex");
      const manifest: DevelopmentHelperManifest = {
        schema_version: 1, component: "ctld", app_version: helper_version, bundle_id: `dev.${sha256}`,
        git_revision: build.source_revision, target: host, bundle_identifier: helper_identifier,
        team_identifier: team, signing_mode: "development", notarized: false,
        archive, sha256, archive_size: bytes.length,
        development: { source_fingerprint: build.source_fingerprint, dirty: build.dirty },
      };
      await writeFile(join(payload, `ctld-${host}.json`), `${JSON.stringify(manifest, null, 2)}\n`);
      console.log("Building ctl with ctld embedded…");
      const cli_artifact = binaryArtifact((await invoke("cargo", [...buildArgs, "-p", "ctl-cli"], {
        CTL_BUNDLED_CTLD_DIR: payload, CTL_BUNDLED_CTLD_MODE: "development",
      })).stdout, "ctl");
      await regularFile(cli_artifact);
      const cli = join(temporary, "ctl");
      await copyFile(cli_artifact, cli);
      await chmod(cli, 0o755);
      await invoke("lipo", ["-verify_arch", architecture, cli]);
      await invoke("codesign", ["--force", "--options", "runtime", "--identifier", cli_identifier, "--sign", identity, cli], {}, true);
      const requirement = `=anchor apple generic and identifier "${cli_identifier}" and certificate leaf[subject.OU] = "${team}"`;
      await invoke("codesign", ["--verify", "--strict", "--test-requirement", requirement, cli]);
      const output = join(output_directory, "ctl");
      // Atomic file replacement leaves the previous CLI usable if any earlier step fails.
      await rename(cli, output);
      console.log(`Signed development CLI: ${output}`);
      return output;
    } finally {
      await rm(temporary, { recursive: true, force: true });
    }
  });
}

async function main(): Promise<void> {
  if (process.platform !== "darwin") throw new Error("signed CLI development is only available on macOS");
  const args = process.argv.slice(2);
  if (args.length > 1 || (args.length === 1 && args[0] !== "--provision")) {
    throw new Error("usage: node scripts/dev/ctl-signed.mts [--provision]");
  }
  if (args[0] === "--provision") {
    await openProvisioningProject({ repository_root, target_directory: await getCargoTargetDirectory(repository_root) });
  } else {
    await buildSignedDevelopmentCli();
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
