import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { chmod, copyFile, lstat, mkdir, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { parseHelperComponent } from "../shared/protocol-contract.mts";
import { buildDaemons } from "./daemon-build.mts";
import { developmentEnvironment, signDevelopmentHelper } from "./ctld-signed.mts";
import { runMacosCommand, type MacosCommandRunner } from "./macos-provisioning.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

export interface DesktopBuildOptions {
  workspace: string;
  output: string;
  env?: NodeJS.ProcessEnv;
  home_directory?: string;
}

export interface DesktopBuildResult {
  schema_version: 1;
  app_version: string;
  bundle_id: string;
  archive: "ctmux.app.tar.gz" | "ctmux.AppImage";
}

/** Build a checkout's complete desktop payload without installing or launching it. */
export async function buildDesktopApp(
  options: DesktopBuildOptions,
  run: MacosCommandRunner = runMacosCommand,
  build: typeof buildDaemons = buildDaemons,
): Promise<DesktopBuildResult> {
  const workspace = await realpath(options.workspace);
  const output_metadata = await lstat(options.output);
  if (!output_metadata.isDirectory() || output_metadata.isSymbolicLink() ||
    (process.getuid && output_metadata.uid !== process.getuid()) || (output_metadata.mode & 0o077) !== 0 ||
    (await readdir(options.output)).length !== 0) {
    throw new Error("desktop build output must be an empty owned private directory");
  }
  const output = await realpath(options.output);
  const env = developmentEnvironment(options.env ?? process.env);
  const invoke = (command: string, args: string[], extra_env: NodeJS.ProcessEnv = {}) =>
    run(command, args, { cwd: workspace, env: { ...env, ...extra_env }, stream_output: command === "pnpm" });
  const metadata = JSON.parse((await invoke("cargo", ["metadata", "--no-deps", "--format-version", "1"])).stdout) as {
    workspace_root?: unknown; target_directory?: unknown; packages?: { name: string; version: string }[];
  };
  if (typeof metadata.workspace_root !== "string" || await realpath(metadata.workspace_root) !== workspace ||
    typeof metadata.target_directory !== "string" || !isAbsolute(metadata.target_directory)) {
    throw new Error("Cargo metadata must describe the requested desktop workspace and an absolute target directory");
  }
  const native = /^host: (\S+)$/m.exec((await invoke(env.RUSTC ?? env.CARGO_BUILD_RUSTC ?? "rustc", ["-vV"])).stdout)?.[1];
  if (!native || !/^(?:x86_64|aarch64)-(?:apple-darwin|unknown-linux-gnu)$/.test(native)) {
    throw new Error("local desktop builds require a native macOS or Linux GNU compiler");
  }
  const app_version = metadata.packages?.find((pkg) => pkg.name === "ctmux-app")?.version;
  const tauri_directory = join(workspace, "apps/desktop/src-tauri");
  const config = JSON.parse(await readFile(join(tauri_directory, "tauri.conf.json"), "utf8")) as {
    productName?: string; version?: string;
  };
  if (!app_version || config.version !== app_version || config.productName !== "ctmux") {
    throw new Error("desktop Cargo package and Tauri config must describe the same ctmux version");
  }

  await mkdir(metadata.target_directory, { recursive: true });
  const target_directory = await realpath(metadata.target_directory);
  const build_root = join(target_directory, "ctl-desktop-build");
  await ensurePrivateDirectory(build_root);
  const checkout = join(build_root, createHash("sha256").update(workspace).digest("hex").slice(0, 20));
  await ensurePrivateDirectory(checkout);
  const cache = join(checkout, "cargo");
  await ensurePrivateDirectory(cache);
  const build_env = { ...env, CARGO_TARGET_DIR: cache };
  console.error("Building native desktop services…");
  const daemons = await build(["--target", native], workspace, build_env);
  const binaries = join(output, "binaries");
  await ensurePrivateDirectory(binaries);
  for (const name of ["ctld", "ctmuxd", "ctl-taskd"] as const) {
    await requireFile(daemons[name]);
    await copyFile(daemons[name], join(binaries, `${name}-${native}`));
    await chmod(join(binaries, `${name}-${native}`), 0o700);
  }

  const macos = native.endsWith("-apple-darwin");
  const external_names = macos ? ["ctmuxd", "ctl-taskd"] : ["ctmuxd", "ctl-taskd", "ctld"];
  const bundle: {
    externalBin: string[];
    resources?: Record<string, string>;
    macOS?: { files: Record<string, string> };
  } = { externalBin: external_names.map((name) => join(binaries, name)) };
  const resources = join(tauri_directory, "resources/agent-bundles");
  if (await directoryExists(resources)) bundle.resources = { [resources]: "resources/agent-bundles/" };
  if (macos) {
    const helper = join(output, "helper");
    const artifact = join(binaries, `ctld-${native}`);
    const component = parseHelperComponent((await invoke(artifact, ["--component-info"])).stdout);
    if (component.build.version !== app_version || typeof component.build.source_revision !== "string" ||
      !/^[a-fA-F0-9]{40}$/.test(component.build.source_revision)) {
      throw new Error("locally compiled ctld has invalid source identity or version");
    }
    const app = join(helper, "ctld.app");
    try {
      await signDevelopmentHelper({
        repository_root: workspace, target_directory, artifact,
        target: native, app_version, output_directory: helper, env,
        home_directory: options.home_directory,
      }, run);
    } catch (error) {
      const detail = (error instanceof Error ? error.message : String(error)).split("\n", 1)[0];
      console.error(`ctld provisioning or signing failed, building an unsigned helper. ${detail}`);
      await rm(helper, { recursive: true, force: true });
      await invoke("/bin/sh", ["scripts/ci/package-ctld-app.sh", artifact, app, app_version], {
        CTLD_PROVISIONING_PROFILE: undefined, CTLD_REQUIRE_DISTRIBUTION_SIGNING: "false",
        CTLD_SIGNING_TIMESTAMP: "none",
      });
    }
    bundle.macOS = { files: { "Helpers/ctld.app": app } };
  }
  const tauri_config = join(output, "tauri-build.json");
  await writeFile(tauri_config, JSON.stringify({ bundle }), { mode: 0o600, flag: "wx" });
  const format = macos ? "app" : "appimage";
  const bundle_directory = join(cache, native, "debug/bundle", macos ? "macos" : "appimage");
  // A failed or incomplete invocation must never select an earlier app bundle.
  await rm(bundle_directory, { recursive: true, force: true });
  console.error("Building desktop frontend and native app…");
  await invoke("pnpm", [
    "--workspace-root", "desktop:build", "--debug", "--target", native,
    "--bundles", format, "--config", tauri_config, ...(macos ? ["--no-sign"] : []),
  ], { CARGO_TARGET_DIR: cache });

  const archive = macos ? "ctmux.app.tar.gz" : "ctmux.AppImage";
  const payload = join(output, archive);
  if (macos) {
    const app = join(bundle_directory, "ctmux.app");
    await requireFile(join(app, "Contents/MacOS/ctmux-app"));
    await invoke("tar", ["-czf", payload, "-C", bundle_directory, "ctmux.app"], { COPYFILE_DISABLE: undefined });
  } else {
    const images = (await readdir(bundle_directory)).filter((name) => name.endsWith(".AppImage"));
    if (images.length !== 1) throw new Error("desktop build must produce exactly one native AppImage");
    const image = join(bundle_directory, images[0]!);
    await requireFile(image);
    await copyFile(image, payload);
    await chmod(payload, 0o755);
  }
  await requireFile(payload);
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(payload)) digest.update(chunk);
  const result: DesktopBuildResult = { schema_version: 1, app_version, bundle_id: `local.${digest.digest("hex")}`, archive };
  await writeFile(join(output, "result.json"), `${JSON.stringify(result, null, 2)}\n`, { mode: 0o600, flag: "wx" });
  return result;
}

async function requireFile(path: string): Promise<void> {
  const metadata = await lstat(path);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size === 0) {
    throw new Error("desktop build must produce nonempty regular files");
  }
}

async function directoryExists(path: string): Promise<boolean> {
  try {
    return (await lstat(path)).isDirectory();
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
    throw error;
  }
}

async function main(): Promise<void> {
  const owned_group = process.env.CTLUP_DEVELOPMENT_LIFELINE === "1";
  const canceled = () => process.kill(-process.pid, "SIGKILL");
  if (owned_group) {
    process.stdin.once("end", canceled);
    process.stdin.once("error", canceled);
    process.stdin.resume();
  }
  try {
    const values = new Map<string, string>();
    const args = process.argv.slice(2);
    for (let index = 0; index < args.length; index += 2) {
      const flag = args[index]!, value = args[index + 1];
      if (!["--workspace", "--output"].includes(flag) || value === undefined || values.has(flag)) {
        throw new Error("usage: build-desktop-app.mts --workspace PATH --output DIRECTORY");
      }
      values.set(flag, value);
    }
    const workspace = values.get("--workspace"), output = values.get("--output");
    if (!workspace || !output) throw new Error("workspace and output directory are required");
    await buildDesktopApp({ workspace, output });
  } finally {
    if (owned_group) {
      process.stdin.off("end", canceled);
      process.stdin.off("error", canceled);
      process.stdin.destroy();
    }
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error: unknown) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
