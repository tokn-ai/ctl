import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { packageCtldBundle, type CtldBundleManifest } from "./package-ctld-bundle.mts";

const execute = promisify(execFile);
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const cliIdentifier = "dev.tokn-ai.ctl.cli";
const targets = new Map([
  ["aarch64-apple-darwin", "arm64"],
  ["x86_64-apple-darwin", "x86_64"],
]);

export interface BuildCtlOptions {
  target: string;
  app_version: string;
  git_revision: string;
  output_directory: string;
  /** Reuse an already signed, notarized, and stapled helper release. */
  ctld_assets?: string;
  signing_identity_path?: string;
  env?: NodeJS.ProcessEnv;
}

export interface BuildContext {
  cwd: string;
  env: NodeJS.ProcessEnv;
}

export type BuildRunner = (
  command: string, args: string[], context: BuildContext,
) => Promise<{ stdout: string; stderr: string }>;

export interface CtlBundleManifest {
  schema_version: 1;
  component: "ctl-cli";
  app_version: string;
  bundle_id: string;
  git_revision: string;
  target: string;
  team_identifier: string;
  signing_mode: "signed";
  notarized: true;
  bundled_ctld_sha256: string;
  archive: string;
  sha256: string;
  archive_size: number;
}

export function binaryArtifact(output: string, name: string): string {
  const paths = new Set<string>();
  for (const line of output.split("\n")) {
    let message;
    try { message = JSON.parse(line); } catch { continue; }
    if (message?.reason === "compiler-artifact" && message.target?.name === name &&
      message.target.kind?.includes("bin") && message.profile?.test !== true &&
      typeof message.executable === "string") {
      paths.add(resolve(repositoryRoot, message.executable));
    }
  }
  if (paths.size !== 1) throw new Error(`Cargo did not report one executable for ${name}`);
  return [...paths][0]!;
}

async function regularFile(path: string): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.size === 0) throw new Error(`expected a nonempty regular file: ${path}`);
}

async function readHelper(directory: string, options: BuildCtlOptions): Promise<CtldBundleManifest> {
  const path = join(directory, `ctld-${options.target}.json`);
  await regularFile(path);
  if ((await lstat(path)).size > 16 * 1024) throw new Error("oversized ctld manifest");
  const manifest = JSON.parse(await readFile(path, "utf8")) as CtldBundleManifest;
  if (manifest.schema_version !== 1 || manifest.component !== "ctld" ||
    manifest.target !== options.target || manifest.app_version !== options.app_version ||
    manifest.bundle_id !== options.app_version || manifest.git_revision !== options.git_revision ||
    manifest.bundle_identifier !== "dev.tokn-ai.ctl.ctld" ||
    typeof manifest.team_identifier !== "string" || !/^[A-Z0-9]{10}$/.test(manifest.team_identifier) || manifest.signing_mode !== "signed" ||
    manifest.notarized !== true || typeof manifest.sha256 !== "string" || !/^[a-f0-9]{64}$/.test(manifest.sha256) ||
    manifest.archive !== `ctld-${options.app_version}-${options.target}.app.tar.gz` ||
    !Number.isSafeInteger(manifest.archive_size) || manifest.archive_size <= 0 ||
    manifest.archive_size > 128 * 1024 * 1024) {
    throw new Error("ctld payload must match the signed, immutable CLI release");
  }
  const archive = join(directory, manifest.archive);
  await regularFile(archive);
  if ((await lstat(archive)).size !== manifest.archive_size ||
    createHash("sha256").update(await readFile(archive)).digest("hex") !== manifest.sha256) {
    throw new Error("ctld payload checksum or size mismatch");
  }
  return manifest;
}

async function verifyReusedHelper(
  directory: string,
  manifest: CtldBundleManifest,
  temporary: string,
  invoke: (command: string, args: string[]) => Promise<{ stdout: string; stderr: string }>,
): Promise<void> {
  const archive = join(directory, manifest.archive);
  // Validate names/types/expanded size before extracting caller-supplied
  // assets. Release helpers use simple ustar paths and contain no links.
  const names = (await invoke("tar", ["-tzf", archive])).stdout.trimEnd().split("\n");
  const listing = (await invoke("tar", ["-tzvf", archive])).stdout.trimEnd().split("\n");
  let expanded = 0;
  if (names.length === 0 || names.length > 256 || new Set(names).size !== names.length || listing.length !== names.length ||
    names.some((name) => !/^ctld\.app(?:\/[a-zA-Z0-9._-]+)*\/?$/.test(name) || name.split("/").some((part) => part === "." || part === ".."))) {
    throw new Error("unsafe ctld archive paths or entry count");
  }
  for (const entry of listing) {
    // BSD tar lists link count, user, group, size; GNU tar lists user/group,
    // size. Both distinguish regular files/directories from links by type.
    const size = /^[-d]\S*\s+\d+\s+\S+\s+\S+\s+(\d+)\s/.exec(entry)?.[1] ??
      /^[-d]\S*\s+\S+\s+(\d+)\s/.exec(entry)?.[1];
    if (size === undefined || !Number.isSafeInteger(Number(size))) throw new Error("unsupported ctld archive entry");
    expanded += Number(size);
    if (expanded > 512 * 1024 * 1024) throw new Error("oversized ctld archive contents");
  }
  const extracted = join(temporary, "helper-check");
  await mkdir(extracted);
  await invoke("tar", ["-xzf", archive, "-C", extracted]);
  const app = join(extracted, "ctld.app");
  const requirement = `=anchor apple generic and identifier "dev.tokn-ai.ctl.ctld" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "${manifest.team_identifier}"`;
  await regularFile(join(app, "Contents", "embedded.provisionprofile"));
  await regularFile(join(app, "Contents", "CodeResources"));
  await invoke("codesign", ["--verify", "--strict", "--test-requirement", requirement, app]);
  await invoke("xcrun", ["stapler", "validate", app]);
  await invoke("spctl", ["--assess", "--type", "execute", "--verbose=2", app]);
  await invoke("lipo", [join(app, "Contents", "MacOS", "ctld"), "-verify_arch", targets.get(manifest.target)!]);
  const info = JSON.parse((await invoke("plutil", ["-convert", "json", "-o", "-", join(app, "Contents", "Info.plist")])).stdout);
  if (info?.CFBundleIdentifier !== "dev.tokn-ai.ctl.ctld" || info.CFBundleExecutable !== "ctld" ||
    info.CFBundleShortVersionString !== manifest.app_version || info.CFBundleVersion !== manifest.app_version) {
    throw new Error("ctld signed bundle metadata does not match its release");
  }
}

/** Compile the helper before embedding its final signed app in the CLI. */
export async function buildCtlBundle(
  options: BuildCtlOptions,
  run: BuildRunner = async (command, args, context) => {
    const result = await execute(command, args, { ...context, maxBuffer: 32 * 1024 * 1024 });
    if (result.stderr) process.stderr.write(result.stderr);
    return result;
  },
): Promise<CtlBundleManifest> {
  const architecture = targets.get(options.target);
  if (!architecture || !/^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?(?:\+[a-zA-Z0-9.-]+)?$/.test(options.app_version) ||
    !/^[a-f0-9]{40}$/.test(options.git_revision)) {
    throw new Error("expected a macOS target, release version, and Git revision");
  }
  const env: NodeJS.ProcessEnv = {
    ...(options.env ?? process.env), CTL_BUNDLED_CTLD_DIR: undefined, CTL_BUNDLED_CTLD_MODE: undefined,
    CTLD_SIGNING_TIMESTAMP: undefined,
  };
  for (const key of ["APPLE_API_KEY_PATH", "APPLE_API_KEY", "APPLE_API_ISSUER"]) {
    if (!env[key]) throw new Error(`signed CLI releases require ${key}`);
  }
  await regularFile(env.APPLE_API_KEY_PATH!);
  const invoke = (command: string, args: string[], extra_env: NodeJS.ProcessEnv = {}) =>
    run(command, args, { cwd: repositoryRoot, env: { ...env, ...extra_env } });
  const revision = (await invoke("git", ["rev-parse", "HEAD"])).stdout.trim();
  if (revision !== options.git_revision || (await invoke("git", ["status", "--porcelain"])).stdout.trim()) {
    throw new Error("signed CLI releases require a clean checkout of the requested revision");
  }
  const metadata = JSON.parse((await invoke("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"])).stdout);
  for (const name of ["ctld", "ctl-cli"]) {
    if (!metadata.packages?.some((pkg: { name: string; version: string }) => pkg.name === name && pkg.version === options.app_version)) {
      throw new Error(`${name} Cargo version does not match the requested release`);
    }
  }
  await mkdir(options.output_directory, { recursive: true });
  if ((await readdir(options.output_directory)).length) throw new Error("CLI output directory must be empty");
  const temporary = await mkdtemp(join(tmpdir(), "ctl-build-bundle-"));
  try {
    const payload = join(temporary, "payload");
    await mkdir(payload);
    let identityPath = options.signing_identity_path ?? env.CTLD_SIGNING_IDENTITY_OUTPUT;
    if (options.ctld_assets) {
      const original = await readHelper(resolve(options.ctld_assets), options);
      // Snapshot the complete inputs so another build cannot replace them while
      // Cargo reads and embeds the bundle.
      await copyFile(join(options.ctld_assets, `ctld-${options.target}.json`), join(payload, `ctld-${options.target}.json`));
      await copyFile(join(options.ctld_assets, original.archive), join(payload, original.archive));
    } else {
      identityPath = join(temporary, "signing-identity");
      const artifact = binaryArtifact((await invoke("cargo", [
        "build", "--locked", "--release", "--target", options.target, "-p", "ctld", "--message-format=json-render-diagnostics",
      ])).stdout, "ctld");
      const app = join(temporary, "ctld.app");
      await invoke("/bin/sh", ["scripts/ci/package-ctld-app.sh", artifact, app, options.app_version], {
        CTLD_REQUIRE_DISTRIBUTION_SIGNING: "true", CTLD_SIGNING_TIMESTAMP: "secure",
        CTLD_SIGNING_IDENTITY_OUTPUT: identityPath,
      });
      await packageCtldBundle({
        target: options.target, app_version: options.app_version, bundle_id: options.app_version,
        git_revision: options.git_revision, input_app: app, output_directory: payload,
        notary_key_path: env.APPLE_API_KEY_PATH!, notary_key_id: env.APPLE_API_KEY!, notary_issuer: env.APPLE_API_ISSUER!,
      }, (command, args) => invoke(command, args));
    }
    const helper = await readHelper(payload, options);
    if (options.ctld_assets) await verifyReusedHelper(payload, helper, temporary, (command, args) => invoke(command, args));
    if (!identityPath) throw new Error("a matching Developer ID signing identity file is required");
    await regularFile(identityPath);
    const identity = (await readFile(identityPath, "utf8")).trim();
    if (!/^[A-Fa-f0-9]{40}$/.test(identity)) throw new Error("invalid Developer ID certificate fingerprint");
    const executable = binaryArtifact((await invoke("cargo", [
      "build", "--locked", "--release", "--target", options.target, "-p", "ctl-cli", "--message-format=json-render-diagnostics",
    ], { CTL_BUNDLED_CTLD_DIR: payload, CTL_BUNDLED_CTLD_MODE: "signed" })).stdout, "ctl");
    const staging = join(temporary, "cli");
    await mkdir(staging);
    const cli = join(staging, "ctl");
    await copyFile(executable, cli);
    await chmod(cli, 0o755);
    await invoke("lipo", [cli, "-verify_arch", architecture]);
    await invoke("codesign", ["--force", "--timestamp", "--options", "runtime", "--identifier", cliIdentifier, "--sign", identity, cli]);
    const requirement = `=anchor apple generic and identifier "${cliIdentifier}" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "${helper.team_identifier}"`;
    await invoke("codesign", ["--verify", "--strict", "--test-requirement", requirement, cli]);
    const zip = join(temporary, "ctl.zip");
    await invoke("ditto", ["-c", "-k", "--keepParent", cli, zip]);
    const notarization = JSON.parse((await invoke("xcrun", [
      "notarytool", "submit", zip, "--key", env.APPLE_API_KEY_PATH!, "--key-id", env.APPLE_API_KEY!,
      "--issuer", env.APPLE_API_ISSUER!, "--wait", "--output-format", "json",
    ])).stdout);
    if (notarization?.status !== "Accepted") throw new Error("bundled CLI notarization was not accepted");
    // Apple cannot staple an individual executable. The embedded ctld.app has
    // its own stapled ticket; the CLI uses Apple's online notarization record.
    await invoke("codesign", ["--verify", "--strict", "--check-notarization", "--test-requirement", "=notarized", cli]);
    const archive = `ctl-${options.app_version}-${options.target}.tar.gz`;
    const archivePath = resolve(options.output_directory, archive);
    await invoke("env", ["COPYFILE_DISABLE=1", "tar", "--format", "ustar", "-czf", archivePath, "-C", staging, "ctl"]);
    await regularFile(archivePath);
    const transported = join(temporary, "transported");
    await mkdir(transported);
    await invoke("tar", ["-xzf", archivePath, "-C", transported]);
    await invoke("codesign", ["--verify", "--strict", "--test-requirement", requirement, join(transported, "ctl")]);
    await invoke("codesign", ["--verify", "--strict", "--check-notarization", "--test-requirement", "=notarized", join(transported, "ctl")]);
    const bytes = await readFile(archivePath);
    const manifest: CtlBundleManifest = {
      schema_version: 1, component: "ctl-cli", app_version: options.app_version, bundle_id: options.app_version,
      git_revision: options.git_revision, target: options.target, team_identifier: helper.team_identifier,
      signing_mode: "signed", notarized: true, bundled_ctld_sha256: helper.sha256,
      archive, sha256: createHash("sha256").update(bytes).digest("hex"), archive_size: bytes.length,
    };
    await writeFile(join(options.output_directory, `${archive}.sha256`), `${manifest.sha256}  ${archive}\n`);
    await writeFile(join(options.output_directory, `ctl-cli-${options.target}.json`), `${JSON.stringify(manifest, null, 2)}\n`);
    return manifest;
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  if (process.platform !== "darwin") throw new Error("signed bundled CLI releases must be built on macOS");
  const args = process.argv.slice(2);
  if (args.length !== 4 && args.length !== 5) {
    throw new Error("usage: build-ctl-bundle.mts TARGET APP_VERSION GIT_REVISION OUTPUT_DIRECTORY [SIGNED_CTLD_ASSETS]");
  }
  const [target, app_version, git_revision, output_directory, ctld_assets] = args;
  const manifest = await buildCtlBundle({ target, app_version, git_revision, output_directory, ctld_assets });
  console.log(`Built ${manifest.archive} with the matching signed ctld.app embedded`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
