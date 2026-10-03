import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

import { maxComponentMetadata, verifyReleaseComponent, type ProtocolInfo } from "../shared/protocol-contract.mts";

const execute = promisify(execFile);
const bundleIdentifier = "dev.tokn-ai.ctl.ctld";
const macTargets = new Map([
  ["x86_64-apple-darwin", "x86_64"],
  ["aarch64-apple-darwin", "arm64"],
]);

export interface CtldBundleOptions {
  target: string;
  app_version: string;
  bundle_id: string;
  git_revision: string;
  input_app: string;
  output_directory: string;
  notary_key_path: string;
  notary_key_id: string;
  notary_issuer: string;
}

export interface CtldBundleManifest {
  schema_version: 1;
  component: "ctld";
  app_version: string;
  bundle_id: string;
  git_revision: string;
  target: string;
  bundle_identifier: "dev.tokn-ai.ctl.ctld";
  team_identifier: string;
  signing_mode: "signed";
  notarized: true;
  archive: string;
  sha256: string;
  archive_size: number;
  protocols: ProtocolInfo[];
}

export type ProcessRunner = (command: string, args: string[]) => Promise<{ stdout: string; stderr: string }>;

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function requireFile(path: string, executable = false): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.size === 0 || (executable && (info.mode & 0o111) === 0)) {
    throw new Error(`expected a nonempty ${executable ? "executable " : ""}regular file: ${path}`);
  }
}

async function validateBundleFiles(directory: string): Promise<void> {
  let entries = 0;
  let bytes = 0;
  const visit = async (path: string): Promise<void> => {
    const info = await lstat(path);
    entries += 1;
    if (info.isDirectory()) {
      for (const name of await readdir(path)) {
        await visit(join(path, name));
      }
    } else if (info.isFile()) {
      bytes += info.size;
    } else {
      throw new Error(`standalone ctld bundles cannot contain links or special files: ${path}`);
    }
    if (entries > 256 || bytes > 512 * 1024 * 1024) {
      throw new Error("ctld bundle exceeds the installer's entry or unpacked size limit");
    }
  };
  await visit(directory);
}

/** A standalone helper must carry the same trusted identity as its profile. */
export function validateDistributionProfile(profile: unknown, now = Date.now()): string {
  if (!record(profile) || !record(profile.Entitlements)) {
    throw new Error("invalid ctld provisioning profile");
  }
  const entitlements = profile.Entitlements;
  const team = entitlements["com.apple.developer.team-identifier"];
  if (
    typeof team !== "string" || !/^[A-Z0-9]{10}$/.test(team) ||
    !Array.isArray(profile.TeamIdentifier) || profile.TeamIdentifier.length !== 1 || profile.TeamIdentifier[0] !== team ||
    entitlements["com.apple.application-identifier"] !== `${team}.${bundleIdentifier}`
  ) {
    throw new Error("ctld provisioning profile has an unexpected application or team identity");
  }
  if (
    profile.ProvisionsAllDevices !== true ||
    ["get-task-allow", "com.apple.security.get-task-allow"].some((key) => entitlements[key] !== undefined && entitlements[key] !== false) ||
    profile.ProvisionedDevices !== undefined
  ) {
    throw new Error("ctld requires a Developer ID distribution profile without get-task-allow");
  }
  const expiration = typeof profile.ExpirationDate === "string" ? Date.parse(profile.ExpirationDate) : NaN;
  if (!Number.isFinite(expiration) || expiration <= now) {
    throw new Error("ctld provisioning profile is expired or has no valid expiration date");
  }
  return team;
}

function signingRequirement(team: string): string {
  return `=anchor apple generic and identifier "${bundleIdentifier}" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "${team}"`;
}

export async function packageCtldBundle(
  options: CtldBundleOptions,
  run: ProcessRunner = async (command, args) => execute(command, args, {
    maxBuffer: args[0] === "--component-info" ? maxComponentMetadata : 16 * 1024 * 1024,
    timeout: args[0] === "--component-info" ? 5_000 : undefined,
    env: { ...process.env, CTLD_ASKPASS: undefined, CTLD_ASKPASS_TOKEN: undefined,
      CTLD_IDENTITY_ASKPASS: undefined, CTLD_IDENTITY_ASKPASS_SOCKET: undefined, CTLD_IDENTITY_ASKPASS_TOKEN: undefined },
  }),
): Promise<CtldBundleManifest> {
  const architecture = macTargets.get(options.target);
  if (!architecture) {
    throw new Error(`unsupported ctld target: ${options.target}`);
  }
  if (!/^[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?(?:\+[a-zA-Z0-9.-]+)?$/.test(options.app_version)) {
    throw new Error("invalid app version");
  }
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._+-]{0,127}$/.test(options.bundle_id)) {
    throw new Error("invalid bundle ID");
  }
  if (!/^[a-f0-9]{40}$/.test(options.git_revision)) {
    throw new Error("git revision must contain 40 lowercase hexadecimal characters");
  }
  if (!options.notary_key_path || !options.notary_key_id || !options.notary_issuer) {
    throw new Error("standalone ctld releases require Apple notarization credentials");
  }
  await requireFile(options.notary_key_path);
  const app = resolve(options.input_app);
  if (basename(app) !== "ctld.app" || !(await lstat(app)).isDirectory()) {
    throw new Error("input must be a ctld.app directory");
  }
  const profilePath = join(app, "Contents", "embedded.provisionprofile");
  const binary = join(app, "Contents", "MacOS", "ctld");
  await Promise.all([
    requireFile(profilePath), requireFile(binary, true), requireFile(join(app, "Contents", "Info.plist")),
  ]);
  await mkdir(options.output_directory, { recursive: true });
  if ((await readdir(options.output_directory)).length !== 0) {
    throw new Error("ctld output directory must be empty");
  }
  const temporary = await mkdtemp(join(tmpdir(), "ctld-notarization-"));
  try {
    const decodedProfile = join(temporary, "profile.plist");
    await run("security", ["cms", "-D", "-i", profilePath, "-o", decodedProfile]);
    // Whole provisioning profiles contain Date/Data values that plutil cannot
    // convert to JSON. Extract the authorization fields individually instead.
    const extract = async (key: string, format = "json"): Promise<string> => {
      if (format === "json") {
        // -extract json still validates Date/Data in the original profile.
        // Isolate the selected value as XML before converting it to JSON.
        const isolated = join(temporary, `extracted-${key}.plist`);
        await run("plutil", ["-extract", key, "xml1", "-o", isolated, decodedProfile]);
        return (await run("plutil", ["-convert", "json", "-o", "-", isolated])).stdout.trim();
      }
      return (await run("plutil", ["-extract", key, format, "-o", "-", decodedProfile])).stdout.trim();
    };
    const profile: Record<string, unknown> = {
      Entitlements: JSON.parse(await extract("Entitlements")),
      TeamIdentifier: JSON.parse(await extract("TeamIdentifier")),
      ProvisionsAllDevices: (await extract("ProvisionsAllDevices", "raw")) === "true",
      ExpirationDate: await extract("ExpirationDate", "raw"),
    };
    try {
      profile.ProvisionedDevices = JSON.parse(await extract("ProvisionedDevices"));
    } catch {
      // Developer ID profiles have no device list; other required fields above
      // still fail closed if extraction is unavailable or the profile is bad.
    }
    const team = validateDistributionProfile(profile);
    const info: unknown = JSON.parse((await run("plutil", ["-convert", "json", "-o", "-", join(app, "Contents", "Info.plist")])).stdout);
    if (
      !record(info) || info.CFBundleIdentifier !== bundleIdentifier || info.CFBundleExecutable !== "ctld" ||
      info.CFBundleShortVersionString !== options.app_version || info.CFBundleVersion !== options.app_version
    ) {
      throw new Error("ctld bundle metadata does not match the requested release version or identity");
    }
    await run("lipo", [binary, "-verify_arch", architecture]);
    const verify = (): ReturnType<ProcessRunner> => run("codesign", [
      "--verify", "--strict", "--verbose=2", "--test-requirement", signingRequirement(team), app,
    ]);
    await verify();
    const signature = await run("codesign", ["-d", "--verbose=4", app]);
    const signatureDetails = `${signature.stdout}\n${signature.stderr}`;
    if (!/^Timestamp=.+$/m.test(signatureDetails) || !/^CodeDirectory .*\(.*\bruntime\b.*\)/m.test(signatureDetails)) {
      throw new Error("ctld must be signed with the hardened runtime and a secure timestamp");
    }
    const signedEntitlements = await run("codesign", ["-d", "--entitlements", ":-", app]);
    const entitlementsPath = join(temporary, "entitlements.plist");
    await writeFile(entitlementsPath, signedEntitlements.stdout);
    const entitlements: unknown = JSON.parse((await run("plutil", ["-convert", "json", "-o", "-", entitlementsPath])).stdout);
    if (
      !record(entitlements) ||
      entitlements["com.apple.application-identifier"] !== `${team}.${bundleIdentifier}` ||
      entitlements["com.apple.developer.team-identifier"] !== team ||
      ["get-task-allow", "com.apple.security.get-task-allow"].some((key) => entitlements[key] !== undefined && entitlements[key] !== false)
    ) {
      throw new Error("ctld signed entitlements do not match its distribution profile");
    }

    const component = verifyReleaseComponent((await run(binary, ["--component-info"])).stdout, options.app_version, options.git_revision);

    // Submit ZIP because Apple's notary service does not accept tar archives.
    // The final release archive is made after stapling so it carries the ticket.
    const submission = join(temporary, "ctld.zip");
    await run("ditto", ["-c", "-k", "--keepParent", app, submission]);
    const response: unknown = JSON.parse((await run("xcrun", [
      "notarytool", "submit", submission, "--key", options.notary_key_path,
      "--key-id", options.notary_key_id, "--issuer", options.notary_issuer,
      "--wait", "--output-format", "json",
    ])).stdout);
    if (!record(response) || response.status !== "Accepted") {
      throw new Error(`ctld notarization was not accepted: ${record(response) ? String(response.status) : "invalid response"}`);
    }
    await run("xcrun", ["stapler", "staple", app]);
    await run("xcrun", ["stapler", "validate", app]);
    await requireFile(join(app, "Contents", "CodeResources"));
    await validateBundleFiles(app);
    await verify();
    await run("spctl", ["--assess", "--type", "execute", "--verbose=2", app]);

    const archive = `ctld-${options.bundle_id}-${options.target}.app.tar.gz`;
    const archivePath = resolve(options.output_directory, archive);
    await run("env", ["COPYFILE_DISABLE=1", "tar", "--format", "ustar", "-czf", archivePath, "-C", dirname(app), "ctld.app"]);
    await requireFile(archivePath);
    const archiveSize = (await lstat(archivePath)).size;
    if (archiveSize > 128 * 1024 * 1024) {
      throw new Error("ctld archive exceeds the installer's download size limit");
    }
    // Check the transported bundle, including the ordinary-file notarization
    // ticket, rather than trusting that the archive preserves Apple's metadata.
    const extracted = join(temporary, "extracted");
    await mkdir(extracted);
    await run("tar", ["-xzf", archivePath, "-C", extracted]);
    const transportedApp = join(extracted, "ctld.app");
    await requireFile(join(transportedApp, "Contents", "CodeResources"));
    await run("xcrun", ["stapler", "validate", transportedApp]);
    await run("codesign", ["--verify", "--strict", "--verbose=2", "--test-requirement", signingRequirement(team), transportedApp]);
    await run("spctl", ["--assess", "--type", "execute", "--verbose=2", transportedApp]);
    const hash = createHash("sha256");
    for await (const chunk of createReadStream(archivePath)) {
      hash.update(chunk);
    }
    const manifest: CtldBundleManifest = {
      schema_version: 1,
      component: "ctld",
      app_version: options.app_version,
      bundle_id: options.bundle_id,
      git_revision: options.git_revision,
      target: options.target,
      bundle_identifier: bundleIdentifier,
      team_identifier: team,
      signing_mode: "signed",
      notarized: true,
      archive,
      sha256: hash.digest("hex"),
      archive_size: archiveSize,
      protocols: component.protocols,
    };
    await writeFile(join(options.output_directory, `${archive}.sha256`), `${manifest.sha256}  ${archive}\n`);
    await writeFile(join(options.output_directory, `ctld-${options.target}.json`), `${JSON.stringify(manifest, null, 2)}\n`);
    return manifest;
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  if (process.platform !== "darwin") {
    throw new Error("standalone ctld releases can only be packaged on macOS");
  }
  const args = process.argv.slice(2);
  if (args.length !== 6) {
    throw new Error("usage: package-ctld-bundle.mts TARGET APP_VERSION BUNDLE_ID GIT_REVISION INPUT_APP OUTPUT_DIRECTORY");
  }
  const [target, app_version, bundle_id, git_revision, input_app, output_directory] = args;
  const manifest = await packageCtldBundle({
    target, app_version, bundle_id, git_revision, input_app, output_directory,
    notary_key_path: process.env.APPLE_API_KEY_PATH ?? "",
    notary_key_id: process.env.APPLE_API_KEY ?? "",
    notary_issuer: process.env.APPLE_API_ISSUER ?? "",
  });
  console.log(`Packaged signed and notarized ${manifest.archive}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
