import { execFile } from "node:child_process";
import { constants } from "node:fs";
import { cp, link, mkdir, mkdtemp, open, readFile, readdir, rm, stat } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { createInterface } from "node:readline/promises";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

const bundle_identifier = "dev.tokn-ai.ctl.ctld";
const team_pattern = /^[A-Z0-9]{10}$/;
const project_template = "ctl/updater/src/updater/sign/ctld/provisioning";

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
  expected_team?: string;
  /** Alternative profile locations allow isolated developer tooling and tests. */
  home_directory?: string;
  now?: Date;
  choose_team?: (teams: string[]) => Promise<string>;
}

export interface ProvisioningProfile {
  path: string;
  team: string;
  expires_at: Date;
  close?: () => Promise<void>;
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
      path.join(options.repository_root, project_template),
      path.dirname(project), { recursive: true },
    );
  }
  console.log(
    "In Xcode, select the ctld-provisioning target, choose your Personal Team " +
    "under Signing & Capabilities, then use Product > Build once.",
  );
  await invoke(options, "open", ["-a", "Xcode", project]);
}

async function configuredProvisioningTeam(options: ProvisioningOptions): Promise<string | undefined> {
  try {
    const contents = await readFile(path.join(provisioningProject(options), "project.pbxproj"), "utf8");
    return /DEVELOPMENT_TEAM\s*=\s*"?([A-Z0-9]{10})"?\s*;/.exec(contents)?.[1];
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    return undefined;
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
  return (await findProvisioningProfiles(options))[0];
}

async function findProvisioningProfiles(options: ProvisioningOptions): Promise<ProvisioningProfile[]> {
  const env = options.env ?? process.env;
  const home = options.home_directory ?? homedir();
  const candidates = env.CTLD_PROVISIONING_PROFILE ? [path.resolve(env.CTLD_PROVISIONING_PROFILE)] : [
    path.join(home, "Library/Application Support/ctmux/signing/ctld.provisionprofile"),
    ...await profileFiles(path.join(home, "Library/Developer/Xcode/UserData/Provisioning Profiles")),
    ...await profileFiles(path.join(home, "Library/MobileDevice/Provisioning Profiles")),
  ];
  await mkdir(options.target_directory, { recursive: true, mode: 0o700 });
  const inspection = await mkdtemp(path.join(options.target_directory, "ctld-profile-"));
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
        const team = application.split(".", 1)[0] ?? "";
        if (!team_pattern.test(team) || application !== `${team}.${bundle_identifier}` ||
          (options.expected_team && team !== options.expected_team)) continue;
        const expires = new Date(await plistValue(":ExpirationDate"));
        if (!Number.isNaN(expires.valueOf()) && expires > (options.now ?? new Date())) {
          matches.push({ path: candidate, team, expires_at: expires });
        }
      } catch {
        // Xcode directories also contain unrelated, unreadable, or stale profiles.
      }
    }
    matches.sort((left, right) => right.expires_at.valueOf() - left.expires_at.valueOf());
    return matches;
  } finally {
    await rm(inspection, { recursive: true, force: true });
  }
}

/** Reuse Xcode's profile and refresh a configured Personal Team when it expires. */
export async function prepareProvisioningProfile(options: ProvisioningOptions): Promise<ProvisioningProfile> {
  const override = (options.env ?? process.env).CTLD_PROVISIONING_PROFILE;
  const settings = signingDirectory(options);
  const saved_team = override || options.expected_team ? undefined : await readTeam(settings);
  const configured_team = options.expected_team ? undefined : await configuredProvisioningTeam(options);
  let team = options.expected_team ?? saved_team ?? (override ? undefined : configured_team);
  const profiles = await findProvisioningProfiles({ ...options, expected_team: team });
  if (!team && !override) {
    const teams = [...new Set(profiles.map((profile) => profile.team))].sort();
    team = teams.length > 1 ? await (options.choose_team ?? chooseTeam)(teams) : teams[0];
    if (team && !teams.includes(team)) throw new Error("Choose one of the listed Apple Teams.");
  }
  let profile = profiles.find((profile) => !team || profile.team === team);
  const renewal = { ...options, expected_team: team };
  if (!profile && configured_team && (override || configured_team === team)) {
    await refreshProfile(options, provisioningProject(options), path.join(options.target_directory, "ctld-provisioning-derived"));
    profile = await findProvisioningProfile(renewal);
  } else if (!profile && !override && team) {
    profile = await renewInstalledProfile(renewal);
  }
  if (!profile) {
    const required_identity = options.expected_team ? `${bundle_identifier} for the installed helper's Apple Team` : bundle_identifier;
    throw new Error(
      `No unexpired provisioning profile authorizes ${required_identity}. ` +
      `Run \`${options.provision_command ?? "pnpm ctld:provision"}\`, ` +
      "select your Personal Team under Signing & Capabilities, and build the target once. " +
      "Free Personal Team profiles expire after seven days.",
    );
  }
  if (!override && !options.expected_team && !saved_team) {
    try {
      await saveTeam(settings, profile.team);
    } catch (error) {
      await profile.close?.();
      throw error;
    }
  }
  return profile;
}

function signingDirectory(options: ProvisioningOptions): string {
  const home = options.home_directory ?? homedir();
  const profile = (options.env ?? process.env).CTL_DATA_DIR || path.join(home, ".tokn/ctl");
  return path.join(profile, "ctlup/signing");
}

async function readTeam(settings: string): Promise<string | undefined> {
  let file;
  try {
    file = await open(path.join(settings, "provisioning-team"), constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
  try {
    const info = await file.stat();
    if (!info.isFile() || info.size > 11 || (info.mode & 0o077) !== 0 || (process.getuid && info.uid !== process.getuid())) {
      throw new Error("The saved Apple Team selection must be a private regular file.");
    }
    const buffer = Buffer.alloc(12);
    const { bytesRead } = await file.read(buffer);
    const team = buffer.toString("utf8", 0, bytesRead).replace(/\n$/, "");
    if (!team_pattern.test(team)) throw new Error("Invalid saved Apple Team selection.");
    return team;
  } finally {
    await file.close();
  }
}

async function saveTeam(settings: string, team: string): Promise<void> {
  await mkdir(path.dirname(settings), { recursive: true, mode: 0o700 });
  await ensurePrivateDirectory(settings);
  const temporary = await mkdtemp(path.join(settings, ".provisioning-team-"));
  try {
    const selection = path.join(temporary, "team");
    const file = await open(selection, "wx", 0o600);
    try {
      await file.writeFile(`${team}\n`);
      await file.sync();
    } finally {
      await file.close();
    }
    try {
      await link(selection, path.join(settings, "provisioning-team"));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
      if (await readTeam(settings) !== team) {
        throw new Error("Another ctlup run chose a different Apple Team. Rerun signing.");
      }
    }
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function chooseTeam(teams: string[]): Promise<string> {
  if (!process.stdin.isTTY || !process.stderr.isTTY) {
    throw new Error("Multiple Apple Teams are available. Run pnpm ctld:provision to choose a team in Xcode or set CTLD_PROVISIONING_PROFILE.");
  }
  console.error("Choose the Apple Team to provision ctld for this Mac.");
  teams.forEach((team, index) => console.error(`  ${index + 1}. ${team}`));
  const terminal = createInterface({ input: process.stdin, output: process.stderr });
  const keep_open = new AbortController();
  terminal.once("close", () => keep_open.abort());
  terminal.once("SIGINT", () => terminal.close());
  try {
    const answer = (await terminal.question("Team number: ", { signal: keep_open.signal })).trim();
    const team = /^\d+$/.test(answer) ? teams[Number(answer) - 1] : undefined;
    if (!team) throw new Error("Choose one of the listed Apple Team numbers.");
    return team;
  } finally {
    terminal.close();
  }
}

async function renewInstalledProfile(options: ProvisioningOptions): Promise<ProvisioningProfile | undefined> {
  // The installed helper already identifies the Apple Team. Use a private
  // copy so renewal can run without a configured checkout or a Cargo build.
  const scratch = await mkdtemp(path.join(options.target_directory, "ctld-provision-renewal-"));
  const close = () => rm(scratch, { recursive: true, force: true });
  let retained = false;
  try {
    const project = path.join(scratch, "ctld-provisioning.xcodeproj");
    const products = path.join(scratch, "products");
    await cp(path.join(options.repository_root, project_template), scratch, { recursive: true });
    try {
      await refreshProfile(options, project, path.join(scratch, "derived"), [
        `DEVELOPMENT_TEAM=${options.expected_team}`, `CONFIGURATION_BUILD_DIR=${products}`,
      ]);
    } catch (cause) {
      const detail = cause instanceof Error ? cause.message : String(cause);
      throw new Error(
        "Xcode could not renew ctld provisioning. Sign into Xcode with the installed helper's Apple Team " +
        `or run \`pnpm ctld:provision\` with that team.\n${detail}`,
        { cause },
      );
    }
    const embedded = path.join(products, "ctld-provisioning.app/Contents/embedded.provisionprofile");
    const profile = await findProvisioningProfile({
      ...options, env: { ...(options.env ?? process.env), CTLD_PROVISIONING_PROFILE: embedded },
    });
    if (profile) {
      retained = true;
      return { ...profile, close };
    }
    return await findProvisioningProfile(options);
  } finally {
    if (!retained) await close();
  }
}

async function refreshProfile(options: ProvisioningOptions, project: string, derived: string, settings: string[] = []): Promise<void> {
  console.error("Refreshing ctld provisioning with Xcode…");
  await invoke(options, "xcodebuild", [
    "-project", project, "-scheme", "ctld-provisioning", "-configuration", "Debug",
    "-destination", "platform=macOS", "-derivedDataPath", derived,
    "-allowProvisioningUpdates", "-allowProvisioningDeviceRegistration", ...settings, "build",
  ], true);
}
