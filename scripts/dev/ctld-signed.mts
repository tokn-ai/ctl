import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseHelperComponent, sameProtocols, type ProtocolInfo } from "../shared/protocol-contract.mts";
import { binaryArtifact } from "../ci/build-ctl-bundle.mts";
import {
  getCargoTargetDirectory, openProvisioningProject, prepareProvisioningProfile,
  runMacosCommand, type MacosCommandRunner,
} from "./macos-provisioning.mts";
import { withPreparationLock } from "./signed-preparation-lock.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";
import { publishDevelopmentHelper } from "./development-helper.mts";

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

export interface DevelopmentBuildOptions {
  repository_root?: string;
  env?: NodeJS.ProcessEnv;
  home_directory?: string;
}

async function regularFile(path: string): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.size === 0) throw new Error(`expected a nonempty regular file: ${path}`);
}

/** Compile and sign the ctld component consumed by debug CLI and GUI builds. */
export async function buildSignedDevelopmentHelper(
  options: DevelopmentBuildOptions = {}, run: MacosCommandRunner = runMacosCommand,
): Promise<string> {
  const root = await realpath(options.repository_root ?? repository_root);
  const env: NodeJS.ProcessEnv = {
    ...(options.env ?? process.env), CTL_BUNDLED_CTLD_DIR: undefined, CTL_BUNDLED_CTLD_MODE: undefined,
    CTLD_REQUIRE_DISTRIBUTION_SIGNING: undefined, CTLD_SIGNING_IDENTITY_OUTPUT: undefined,
    CTLD_SIGNING_TIMESTAMP: undefined,
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
  if (!host || !architecture) throw new Error("signed development builds require a native macOS Rust target");
  const metadata = JSON.parse((await invoke("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"], {}, false, 30_000)).stdout);
  const helper_version = metadata.packages?.find((pkg: { name: string }) => pkg.name === "ctld")?.version;
  if (typeof helper_version !== "string" ||
    !/^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?(?:\+[a-zA-Z0-9.-]+)?$/.test(helper_version) ||
    typeof metadata.target_directory !== "string" || !isAbsolute(metadata.target_directory)) {
    throw new Error("Cargo must report a valid ctld version and an absolute target directory");
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
      // Keep signed component and ordinary Cargo builds in separate caches. The preparation
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
      const component = parseHelperComponent((await invoke(artifact, ["--component-info"], {}, false, 10_000)).stdout);
      const build = component.build;
      if (build.version !== helper_version || typeof build.source_revision !== "string" || !/^[a-f0-9]{40}$/.test(build.source_revision)) {
        throw new Error("locally compiled ctld has invalid source identity or version");
      }
      const app = join(temporary, "ctld.app");
      const identity_path = join(temporary, "signing-identity");
      console.log("Signing ctld with your provisioning profile…");
      await invoke("/bin/sh", ["scripts/ci/package-ctld-app.sh", artifact, app, helper_version], {
        CTLD_PROVISIONING_PROFILE: profile.path, CTLD_REQUIRE_DISTRIBUTION_SIGNING: "false",
        CTLD_SIGNING_IDENTITY_OUTPUT: identity_path, CTLD_SIGNING_TIMESTAMP: "none",
      }, true);
      for (const relative of ["Contents/MacOS/ctld", "Contents/Info.plist", "Contents/embedded.provisionprofile", "Contents/_CodeSignature/CodeResources"]) {
        await regularFile(join(app, relative));
      }
      await invoke("lipo", [join(app, "Contents/MacOS/ctld"), "-verify_arch", architecture]);
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
        protocols: component.protocols,
      };
      const receipt = `${JSON.stringify(manifest, null, 2)}\n`;
      if (Buffer.byteLength(receipt) > 16 * 1024) throw new Error("ctld development receipt exceeds the install limit");
      await writeFile(join(payload, `ctld-${host}.json`), receipt, { mode: 0o600 });
      const helper = await publishDevelopmentHelper({
        repository_root: root, target_directory, app, manifest,
      }, async (published_app, receipt) => {
        for (const relative of ["Contents/MacOS/ctld", "Contents/Info.plist", "Contents/embedded.provisionprofile", "Contents/_CodeSignature/CodeResources"]) {
          await regularFile(join(published_app, relative));
        }
        const executable = join(published_app, "Contents/MacOS/ctld");
        await invoke("lipo", [executable, "-verify_arch", architecture]);
        const requirement = `=anchor apple generic and identifier "${helper_identifier}" and certificate leaf[subject.OU] = "${receipt.team_identifier}"`;
        await invoke("codesign", ["--verify", "--strict", "--test-requirement", requirement, published_app]);
        const published_component = parseHelperComponent((await invoke(executable, ["--component-info"], {}, false, 10_000)).stdout);
        if (published_component.build.version !== receipt.app_version ||
          published_component.build.source_revision !== receipt.git_revision ||
          published_component.build.source_fingerprint !== receipt.development.source_fingerprint ||
          published_component.build.dirty !== receipt.development.dirty ||
          !sameProtocols(published_component.protocols, receipt.protocols)) {
          throw new Error("signed development helper metadata does not match its receipt");
        }
      });
      console.log(`Signed development helper: ${helper}`);
      console.log(`From ${root}: cargo run -p ctl-cli --target-dir ${shellQuote(target_directory)} -- passwords`);
      console.log(`From ${root}: pnpm desktop:dev`);
      return helper;
    } finally {
      await rm(temporary, { recursive: true, force: true });
    }
  });
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log("usage: pnpm ctld:provision | pnpm ctld:build");
    console.log("  ctld:provision  open the shared Xcode provisioning project");
    console.log("  ctld:build      build and select a signed development ctld.app");
    return;
  }
  if (args.length > 1 || (args.length === 1 && args[0] !== "--provision")) {
    throw new Error("usage: pnpm ctld:provision | pnpm ctld:build");
  }
  if (process.platform !== "darwin") throw new Error("signed helper development is only available on macOS");
  if (args[0] === "--provision") {
    await openProvisioningProject({ repository_root, target_directory: await getCargoTargetDirectory(repository_root) });
  } else {
    await buildSignedDevelopmentHelper();
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
