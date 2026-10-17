// The same entry point is used by local development and the CI jobs.
import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const toolchain = readFileSync(resolve(root, "rust-toolchain.toml"), "utf8")
  .match(/^channel\s*=\s*"([^"\n]+)"/m)?.[1];
if (!toolchain) throw new Error("rust-toolchain.toml must declare a channel");
const scopes = ["scripts", "quality", "rust", "frontend", "packages"] as const;
type Scope = typeof scopes[number];
const [requested, ...options] = process.argv.slice(2);
if (![...scopes, "all"].includes(requested) || options.some((option) => {
  if (option === "--allow-dirty") return !["all", "packages"].includes(requested);
  if (option === "--portable") return !["quality", "rust"].includes(requested);
  return true;
})) {
  throw new Error("usage: node scripts/ci/check.mts <scripts|quality|rust|frontend|packages|all> [--allow-dirty (packages/all)] [--portable (quality/rust)]");
}

// Native Windows clients and SSH targets share one checked and executed set.
// ctld, the desktop app, and the macOS Keychain bridge have separate platform
// requirements; all portable workspace packages belong here, including libraries.
const portable_packages = [
  "tokn-ctl-core", "ctl-cli", "ctl-client", "ctl-proto", "ctl-agent", "ctl-ipc",
  "ctmux-process-info", "ctmux-client", "ctmux-cli", "ctmux-core", "ctmux-avt",
  "ctmuxd", "ctmux-tui", "ctmux-ipc", "ctmux-proto",
  "ctl-task-proto", "ctl-task-ipc", "ctl-taskd", "ctl-task-cli", "ctl-task-client", "ctl-task-store",
];
const package_args = options.includes("--portable")
  ? portable_packages.flatMap((name) => ["-p", name])
  : ["--workspace"];

function run(command: string, args: string[], cwd = root, rust_toolchain = toolchain): void {
  console.log(`\nChecking: ${command} ${args.join(" ")}`);
  const result = spawnSync(command, args, {
    cwd,
    stdio: "inherit",
    // Packaged-source validation runs outside the checkout, where rustup cannot
    // discover its toolchain file. Keep those builds on the same compiler too.
    env: { ...process.env, RUSTUP_TOOLCHAIN: rust_toolchain },
    shell: process.platform === "win32" && command === "pnpm",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} failed (${result.signal ?? result.status})`);
  }
}

function check(scope: Scope): void {
  switch (scope) {
    case "scripts": {
      const tests = ["scripts/dev", "scripts/ci"].flatMap((directory) =>
        readdirSync(resolve(root, directory)).filter((name) => name.endsWith(".test.mts"))
          .sort().map((name) => `${directory}/${name}`));
      run(process.execPath, ["--experimental-strip-types", "--disable-warning=ExperimentalWarning", "--test", ...tests]);
      break;
    }
    case "quality":
      run("cargo", ["fmt", "--all", "--", "--check"]);
      run("cargo", ["clippy", "--locked", ...package_args, "--all-targets", "--", "-D", "warnings"]);
      break;
    case "rust":
      run("cargo", ["test", "--locked", ...package_args]);
      break;
    case "frontend": {
      for (const args of [
        ["install", "--frozen-lockfile"], ["check"], ["desktop:test"],
        ["--filter", "ctmux-app", "build"],
      ]) {
        run("pnpm", args);
      }
      break;
    }
    case "packages":
      // Keep the published-source gate on the declared minimum supported Rust.
      run(process.execPath, ["--experimental-strip-types", "scripts/ci/verify-cargo-packages.mts", ...options], root, "1.97.0");
      break;
  }
}

for (const scope of requested === "all" ? scopes : [requested as Scope]) check(scope);
