import {
  execFile as execFileCallback,
  spawn,
  type ChildProcess,
} from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { homedir, tmpdir } from "node:os";
import {
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
} from "node:fs/promises";
import path from "node:path";

const execFile = promisify(execFileCallback);
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "../..");
const appDirectory = path.join(repositoryRoot, "apps/rmux");
const bundleIdentifier = "io.rmux.desktop.ctld";
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

  const inspectionDirectory = await mkdtemp(path.join(tmpdir(), "rmux-profile-"));
  let runtimeDirectory: string | undefined;
  let daemon: ChildProcess | undefined;
  let tauri: ChildProcess | undefined;
  try {
    const profile = await findProfile(inspectionDirectory);
    const targetDirectory = path.resolve(
      repositoryRoot,
      process.env.CARGO_TARGET_DIR ?? "target",
    );
    const tauriConfig = JSON.parse(
      await readFile(path.join(appDirectory, "src-tauri/tauri.conf.json"), "utf8"),
    ) as { version?: string };
    if (!tauriConfig.version) {
      throw new Error("rmux has no version in src-tauri/tauri.conf.json");
    }

    await run("cargo", ["build", "--locked", "-p", "ctld"], repositoryRoot);
    const daemonBundle = path.join(targetDirectory, "ctld-app-dev/ctld.app");
    await run(
      path.join(repositoryRoot, "scripts/ci/package-ctld-app.sh"),
      [
        path.join(targetDirectory, "debug/ctld"),
        daemonBundle,
        tauriConfig.version,
      ],
      repositoryRoot,
      {
        CTLD_PROVISIONING_PROFILE: profile.path,
      },
    );

    const daemonExecutable = path.join(daemonBundle, "Contents/MacOS/ctld");
    runtimeDirectory = await mkdtemp(path.join(tmpdir(), "rmux-ctld-dev-"));
    const socket = path.join(runtimeDirectory, "ctld.sock");
    const environment = {
      ...process.env,
      CTLD_BIN: daemonExecutable,
      CTLD_RUNTIME_DIR: runtimeDirectory,
    };
    daemon = spawn(daemonExecutable, ["--socket", socket], {
      cwd: repositoryRoot,
      env: environment,
      stdio: ["ignore", "inherit", "inherit"],
    });
    await waitForSocket(socket, daemon);

    tauri = spawn("pnpm", ["tauri", "dev"], {
      cwd: appDirectory,
      env: environment,
      stdio: "inherit",
    });
    const { code, signal } = await waitForExit(tauri);
    if (code !== 0 && signal === null) {
      throw new Error(`tauri dev exited with status ${code ?? "unknown"}`);
    }
  } finally {
    stop(tauri);
    stop(daemon);
    if (daemon) {
      await Promise.race([waitForExit(daemon), delay(2_000)]);
      if (daemon.exitCode === null && daemon.signalCode === null) {
        daemon.kill("SIGKILL");
        await waitForExit(daemon);
      }
    }
    await rm(inspectionDirectory, { recursive: true, force: true });
    if (runtimeDirectory) {
      await rm(runtimeDirectory, { recursive: true, force: true });
    }
  }
}

async function findProfile(inspectionDirectory: string): Promise<Profile> {
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
  const profile = matches[0];
  if (!profile) {
    throw new Error(
      `No unexpired provisioning profile authorizes *.${bundleIdentifier}. ` +
        `Download one with Xcode or place it at ${localProfile}.`,
    );
  }
  return profile;
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

async function waitForSocket(socket: string, daemon: ChildProcess): Promise<void> {
  const deadline = Date.now() + 3_000;
  while (Date.now() < deadline) {
    if (daemon.exitCode !== null || daemon.signalCode !== null) {
      throw new Error("signed ctld stopped during startup");
    }
    try {
      const metadata = await stat(socket);
      if (metadata.isSocket()) {
        return;
      }
    } catch {
      // The daemon creates its socket asynchronously.
    }
    await delay(25);
  }
  throw new Error("signed ctld did not create its socket within three seconds");
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

function stop(child: ChildProcess | undefined): void {
  if (child?.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
  }
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
