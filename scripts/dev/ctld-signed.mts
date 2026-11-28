import { dirname, isAbsolute, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { runBuildCommand, type BuildCommandRunner } from "./build-command.mts";

const repository_root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

export interface DevelopmentSigningOptions {
  repository_root: string;
  artifact: string;
  app_version: string;
  output_directory: string;
  env: NodeJS.ProcessEnv;
}

/** Delegate signing and provisioning to the updater that launched the build. */
export async function signDevelopmentHelper(
  options: DevelopmentSigningOptions, run: BuildCommandRunner = runBuildCommand,
): Promise<void> {
  const executable = options.env.CTLUP_DEVELOPMENT_EXECUTABLE;
  if (!executable || !isAbsolute(executable)) {
    throw new Error("ctld development signing requires the ctlup that launched the build");
  }
  await run(executable, [
    "sign-development-helper", "--artifact", options.artifact,
    "--output", options.output_directory, "--version", options.app_version, "--non-interactive",
  ], { cwd: options.repository_root, env: options.env, stream_stderr: true });
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

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log("usage: pnpm ctld:build");
    console.log("  ctld:build      prepare the complete native development installation through ctlup");
    return;
  }
  if (args.length) throw new Error("usage: pnpm ctld:build");
  if (process.platform !== "darwin") throw new Error("signed helper development is only available on macOS");
  const { prepareDevelopment } = await import("./ctlup-development.mts");
  const prepared = await prepareDevelopment([], repository_root, process.env);
  await prepared.close();
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
