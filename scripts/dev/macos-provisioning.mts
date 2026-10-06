import { execFile } from "node:child_process";
import { cp, mkdtemp, readFile, readdir, rm, stat } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import path from "node:path";

const bundle_identifier = "dev.tokn-ai.ctl.ctld";

export interface MacosCommandContext {
  cwd: string;
  env: NodeJS.ProcessEnv;
  stream_output?: boolean;
  stream_stderr?: boolean;
  timeout_ms?: number;
}

export type MacosCommandRunner = (
  command: string, args: string[], context: MacosCommandContext,
) => Promise<{ stdout: string; stderr: string }>;

export const runMacosCommand: MacosCommandRunner = (command, args, context) =>
  new Promise((resolve, reject) => {
    const child = execFile(command, args, {
      cwd: context.cwd, env: context.env, timeout: context.timeout_ms,
      maxBuffer: args[0] === "--component-info" ? 16 * 1024 : 32 * 1024 * 1024,
    }, (error, stdout, stderr) => {
      if (error) reject(error);
      else resolve({ stdout, stderr });
    });
    if (context.stream_output) {
      child.stdout?.on("data", (chunk) => process.stdout.write(chunk));
    }
    if (context.stream_output || context.stream_stderr) {
      child.stderr?.on("data", (chunk) => process.stderr.write(chunk));
    }
  });

export interface ProvisioningOptions {
  repository_root: string;
  target_directory: string;
  env?: NodeJS.ProcessEnv;
  run?: MacosCommandRunner;
  provision_command?: string;
  /** Alternative profile locations allow isolated developer tooling and tests. */
  home_directory?: string;
  now?: Date;
}

export interface ProvisioningProfile {
  path: string;
  expires_at: Date;
}

function invoke(options: ProvisioningOptions, command: string, args: string[], stream_output = false) {
  return (options.run ?? runMacosCommand)(command, args, {
    cwd: options.repository_root, env: options.env ?? process.env, stream_output,
  });
}

export async function getCargoTargetDirectory(
  repository_root: string,
  run: MacosCommandRunner = runMacosCommand,
): Promise<string> {
  const { stdout } = await run("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
    cwd: repository_root, env: process.env, timeout_ms: 30_000,
  });
  const metadata = JSON.parse(stdout) as { target_directory?: unknown };
  if (typeof metadata.target_directory !== "string" || !path.isAbsolute(metadata.target_directory)) {
    throw new Error("Cargo did not report an absolute target directory");
  }
  return metadata.target_directory;
}

function provisioningProject(options: ProvisioningOptions): string {
  return path.join(options.target_directory, "ctld-provisioning/ctld-provisioning.xcodeproj");
}

export async function openProvisioningProject(options: ProvisioningOptions): Promise<void> {
  const project = provisioningProject(options);
  try {
    await stat(project);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    await cp(
      path.join(options.repository_root, "scripts/dev/macos/ctld-provisioning"),
      path.dirname(project), { recursive: true },
    );
  }
  console.log(
    "In Xcode, select the ctld-provisioning target, choose your Personal Team " +
    "under Signing & Capabilities, then use Product > Build once.",
  );
  await invoke(options, "open", ["-a", "Xcode", project]);
}

async function configuredProvisioningProject(options: ProvisioningOptions): Promise<boolean> {
  try {
    const contents = await readFile(path.join(provisioningProject(options), "project.pbxproj"), "utf8");
    return /DEVELOPMENT_TEAM = [A-Z0-9]+;/.test(contents);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    return false;
  }
}

async function profileFiles(directory: string): Promise<string[]> {
  try {
    const files: string[] = [];
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const candidate = path.join(directory, entry.name);
      if (entry.isFile()) files.push(candidate);
      else if (entry.isDirectory()) files.push(...await profileFiles(candidate));
    }
    return files;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    return [];
  }
}

export async function findProvisioningProfile(options: ProvisioningOptions): Promise<ProvisioningProfile | undefined> {
  const env = options.env ?? process.env;
  const home = options.home_directory ?? homedir();
  const candidates = env.CTLD_PROVISIONING_PROFILE ? [path.resolve(env.CTLD_PROVISIONING_PROFILE)] : [
    path.join(home, "Library/Application Support/ctmux/signing/ctld.provisionprofile"),
    ...await profileFiles(path.join(home, "Library/Developer/Xcode/UserData/Provisioning Profiles")),
    ...await profileFiles(path.join(home, "Library/MobileDevice/Provisioning Profiles")),
  ];
  const inspection = await mkdtemp(path.join(tmpdir(), "ctld-profile-"));
  try {
    const matches: ProvisioningProfile[] = [];
    for (const [index, candidate] of [...new Set(candidates)].entries()) {
      try {
        const decoded = path.join(inspection, `profile-${index}.plist`);
        await invoke(options, "security", ["cms", "-D", "-i", candidate, "-o", decoded]);
        const plistValue = async (key: string) => (await invoke(options, "/usr/libexec/PlistBuddy", [
          "-c", `Print ${key}`, decoded,
        ])).stdout.trim();
        const application = await plistValue(":Entitlements:com.apple.application-identifier");
        if (!application.endsWith(`.${bundle_identifier}`)) continue;
        const expires = new Date(await plistValue(":ExpirationDate"));
        if (!Number.isNaN(expires.valueOf()) && expires > (options.now ?? new Date())) {
          matches.push({ path: candidate, expires_at: expires });
        }
      } catch {
        // Xcode directories also contain unrelated, unreadable, or stale profiles.
      }
    }
    matches.sort((left, right) => right.expires_at.valueOf() - left.expires_at.valueOf());
    return matches[0];
  } finally {
    await rm(inspection, { recursive: true, force: true });
  }
}

/** Reuse Xcode's profile and refresh a configured Personal Team when it expires. */
export async function prepareProvisioningProfile(options: ProvisioningOptions): Promise<ProvisioningProfile> {
  let profile = await findProvisioningProfile(options);
  if (!profile && await configuredProvisioningProject(options)) {
    console.log("Refreshing ctld provisioning with Xcode…");
    await invoke(options, "xcodebuild", [
      "-project", provisioningProject(options), "-scheme", "ctld-provisioning",
      "-configuration", "Debug", "-destination", "platform=macOS",
      "-derivedDataPath", path.join(options.target_directory, "ctld-provisioning-derived"),
      "-allowProvisioningUpdates", "-allowProvisioningDeviceRegistration", "build",
    ], true);
    profile = await findProvisioningProfile(options);
  }
  if (!profile) {
    throw new Error(
      `No unexpired provisioning profile authorizes ${bundle_identifier}. ` +
      `Run \`${options.provision_command ?? "pnpm provision"}\`, ` +
      "select your Personal Team under Signing & Capabilities, and build the target once. " +
      "Free Personal Team profiles expire after seven days.",
    );
  }
  return profile;
}
