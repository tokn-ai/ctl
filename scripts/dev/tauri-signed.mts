import {
  execFile as execFileCallback,
  spawn,
  type ChildProcess,
} from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { homedir, tmpdir } from "node:os";
import {
  cp,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
} from "node:fs/promises";
import path from "node:path";
import { servePreparation } from "./daemon-preparation.mts";
import { SignedDaemon } from "./signed-daemon.mts";

const execFile = promisify(execFileCallback);
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "../..");
const appDirectory = path.join(repositoryRoot, "apps/desktop");
const bundleIdentifier = "io.rmux.desktop.ctld";
const provisioningTemplate = path.join(
  scriptDirectory,
  "macos/ctld-provisioning",
);
const localProfile = path.join(
  homedir(),
  "Library/Application Support/rmux/signing/ctld.provisionprofile",
);

interface Profile {
  path: string;
  expires_at: Date;
}

async function main(): Promise<void> {
  if (process.platform !== "darwin") {
    throw new Error("signed Tauri development is only available on macOS");
  }

  const targetDirectory = await cargoTargetDirectory();
  if (process.argv.slice(2).includes("--provision")) {
    await openProvisioningProject(targetDirectory);
    return;
  }

  const inspectionDirectory = await mkdtemp(path.join(tmpdir(), "rmux-profile-"));
  let runtimeDirectory: string | undefined;
  let daemon: SignedDaemon | undefined;
  let supervisor: Awaited<ReturnType<typeof servePreparation>> | undefined;
  let tauri: ChildProcess | undefined;
  const stopTauri = () => {
    if (tauri?.pid) {
      try {
        // The launcher owns this process group, including pnpm, Vite and Cargo.
        process.kill(-tauri.pid, "SIGTERM");
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
    }
  };
  try {
    let profile = await findProfile(inspectionDirectory);
    if (!profile && (await configuredProvisioningProject(targetDirectory))) {
      await refreshPersonalTeamProfile(targetDirectory);
      profile = await findProfile(inspectionDirectory);
    }
    if (!profile) {
      throw new Error(
        `No unexpired provisioning profile authorizes ${bundleIdentifier}. ` +
          "Run `pnpm tauri:dev:provision`, select your Personal Team under " +
          "Signing & Capabilities, and build the target once. Free Personal " +
          "Team profiles expire after seven days.",
      );
    }
    const tauriConfig = JSON.parse(
      await readFile(path.join(appDirectory, "src-tauri/tauri.conf.json"), "utf8"),
    ) as { version?: string };
    if (!tauriConfig.version) {
      throw new Error("rmux has no version in src-tauri/tauri.conf.json");
    }

    runtimeDirectory = await mkdtemp(path.join(tmpdir(), "rmux-ctld-dev-"));
    daemon = new SignedDaemon({
      runtime_directory: runtimeDirectory,
      profile_path: profile.path,
      app_version: tauriConfig.version,
      repository_root: repositoryRoot,
    });
    const ownedDaemon = daemon;
    const supervisorSocket = path.join(runtimeDirectory, "prepare.sock");
    supervisor = await servePreparation(supervisorSocket, (executable) => ownedDaemon.prepare(executable));
    const environment = {
      ...process.env,
      CTLD_BIN: daemon.executable,
      CTLD_RUNTIME_DIR: runtimeDirectory,
      CTLD_SOCKET_PATH: daemon.socket_path,
      RMUX_DEV_DAEMON_SUPERVISOR: supervisorSocket,
    };
    tauri = spawn("pnpm", ["tauri", "dev", ...process.argv.slice(2)], {
      cwd: appDirectory,
      env: environment,
      stdio: "inherit",
      detached: true,
    });
    process.on("SIGINT", stopTauri);
    process.on("SIGTERM", stopTauri);
    const { code, signal } = await waitForExit(tauri);
    if (code !== 0 && signal === null) {
      throw new Error(`tauri dev exited with status ${code ?? "unknown"}`);
    }
  } finally {
    stopTauri();
    process.off("SIGINT", stopTauri);
    process.off("SIGTERM", stopTauri);
    await supervisor?.close();
    await daemon?.close();
    await rm(inspectionDirectory, { recursive: true, force: true });
    if (runtimeDirectory) {
      await rm(runtimeDirectory, { recursive: true, force: true });
    }
  }
}

async function cargoTargetDirectory(): Promise<string> {
  const { stdout } = await execFile("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
    cwd: repositoryRoot,
  });
  return (JSON.parse(stdout) as { target_directory: string }).target_directory;
}

function provisioningProject(targetDirectory: string): string {
  return path.join(
    targetDirectory,
    "ctld-provisioning/ctld-provisioning.xcodeproj",
  );
}

async function openProvisioningProject(targetDirectory: string): Promise<void> {
  const project = provisioningProject(targetDirectory);
  if (!(await exists(project))) {
    await cp(provisioningTemplate, path.dirname(project), { recursive: true });
  }
  console.log(
    "In Xcode, select the ctld-provisioning target, choose your Personal Team " +
      "under Signing & Capabilities, then use Product > Build once.",
  );
  await run("open", ["-a", "Xcode", project], repositoryRoot);
}

async function configuredProvisioningProject(
  targetDirectory: string,
): Promise<boolean> {
  try {
    const projectFile = path.join(
      provisioningProject(targetDirectory),
      "project.pbxproj",
    );
    const contents = await readFile(projectFile, "utf8");
    return /DEVELOPMENT_TEAM = [A-Z0-9]+;/.test(contents);
  } catch {
    return false;
  }
}

async function refreshPersonalTeamProfile(
  targetDirectory: string,
): Promise<void> {
  await run(
    "xcodebuild",
    [
      "-project",
      provisioningProject(targetDirectory),
      "-scheme",
      "ctld-provisioning",
      "-configuration",
      "Debug",
      "-destination",
      "platform=macOS",
      "-derivedDataPath",
      path.join(targetDirectory, "ctld-provisioning-derived"),
      "-allowProvisioningUpdates",
      "-allowProvisioningDeviceRegistration",
      "build",
    ],
    repositoryRoot,
  );
}

async function findProfile(
  inspectionDirectory: string,
): Promise<Profile | undefined> {
  const override = process.env.CTLD_PROVISIONING_PROFILE;
  const candidates = override
    ? [path.resolve(override)]
    : [
        localProfile,
        ...(await profileFiles(
          path.join(homedir(), "Library/Developer/Xcode/UserData/Provisioning Profiles"),
        )),
        ...(await profileFiles(
          path.join(homedir(), "Library/MobileDevice/Provisioning Profiles"),
        )),
      ];
  const matches: Profile[] = [];
  for (const [index, candidate] of [...new Set(candidates)].entries()) {
    try {
      const decoded = path.join(inspectionDirectory, `profile-${index}.plist`);
      await execFile("security", ["cms", "-D", "-i", candidate, "-o", decoded]);
      const applicationIdentifier = await plistValue(
        decoded,
        ":Entitlements:com.apple.application-identifier",
      );
      if (!applicationIdentifier.endsWith(`.${bundleIdentifier}`)) {
        continue;
      }
      const expiresAt = new Date(await plistValue(decoded, ":ExpirationDate"));
      if (!Number.isNaN(expiresAt.valueOf()) && expiresAt > new Date()) {
        matches.push({ path: candidate, expires_at: expiresAt });
      }
    } catch {
      // Xcode profile directories can contain unrelated or stale files.
    }
  }
  matches.sort((left, right) => right.expires_at.valueOf() - left.expires_at.valueOf());
  return matches[0];
}

async function exists(candidate: string): Promise<boolean> {
  try {
    await stat(candidate);
    return true;
  } catch {
    return false;
  }
}

async function profileFiles(directory: string): Promise<string[]> {
  try {
    const entries = await readdir(directory, { withFileTypes: true });
    const files: string[] = [];
    for (const entry of entries) {
      const entryPath = path.join(directory, entry.name);
      if (entry.isFile()) {
        files.push(entryPath);
      } else if (entry.isDirectory()) {
        files.push(...(await profileFiles(entryPath)));
      }
    }
    return files;
  } catch {
    return [];
  }
}

async function plistValue(plist: string, key: string): Promise<string> {
  const { stdout } = await execFile("/usr/libexec/PlistBuddy", [
    "-c",
    `Print ${key}`,
    plist,
  ]);
  return stdout.trim();
}

async function run(
  command: string,
  args: string[],
  cwd: string,
  additionalEnvironment: NodeJS.ProcessEnv = {},
): Promise<void> {
  const child = spawn(command, args, {
    cwd,
    env: { ...process.env, ...additionalEnvironment },
    stdio: "inherit",
  });
  const { code, signal } = await waitForExit(child);
  if (code !== 0) {
    throw new Error(
      `${path.basename(command)} exited with ${signal ? `signal ${signal}` : `status ${code}`}`,
    );
  }
}

function waitForExit(
  child: ChildProcess,
): Promise<{ code: number | null; signal: NodeJS.Signals | null }> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
