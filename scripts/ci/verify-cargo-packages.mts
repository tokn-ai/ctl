import { spawnSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

interface CargoPackage {
  name: string;
  version: string;
  manifest_path: string;
  publish: string[] | null;
  license: string | null;
  description: string | null;
  readme: string | null;
  repository: string | null;
  rust_version: string | null;
  dependencies: { name: string; path?: string; req: string }[];
}

interface CargoMetadata {
  packages: CargoPackage[];
  target_directory: string;
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const options = process.argv.slice(2);
if (options.some((option) => !["--allow-dirty", "--offline"].includes(option))) {
  throw new Error("usage: verify-cargo-packages.mts [--allow-dirty] [--offline]");
}
const offline = options.includes("--offline") ? ["--offline"] : [];

function run(command: string, args: string[], cwd = root): void {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed (${result.signal ?? result.status})`);
  }
}

const result = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1", ...offline], {
  cwd: root,
  encoding: "utf8",
});
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(result.stderr);
const metadata = JSON.parse(result.stdout) as CargoMetadata;
const packages = metadata.packages.filter((pkg) => pkg.publish?.length !== 0);
const excluded = metadata.packages.filter((pkg) => pkg.publish?.length === 0);
const license = readFileSync(join(root, "LICENSE"), "utf8");
const package_licenses = new Map<string, string>();
for (const pkg of packages) {
  // The AVT fork retains its upstream Apache license; other family members
  // must continue to carry the workspace MIT license.
  const is_avt = pkg.name === "ctmux-avt";
  const expected_license = is_avt ? "Apache-2.0" : "MIT";
  if (pkg.license !== expected_license || !pkg.description || !pkg.readme || !pkg.repository || !pkg.rust_version) {
    throw new Error(`${pkg.name}: missing publishing metadata`);
  }
  if (pkg.publish?.join() !== "crates-io") {
    throw new Error(`${pkg.name}: expected explicit crates.io publishing policy`);
  }
  const source_license = readFileSync(join(dirname(pkg.manifest_path), "LICENSE"), "utf8");
  if (!is_avt && source_license !== license) {
    throw new Error(`${pkg.name}: LICENSE must match the workspace MIT license`);
  }
  package_licenses.set(pkg.name, source_license);
  for (const dependency of pkg.dependencies.filter((dep) => dep.path)) {
    const member = packages.find((candidate) => candidate.name === dependency.name);
    if (!member || dependency.req !== `=${member.version}`) {
      throw new Error(`${pkg.name}: ${dependency.name} must pin a publishable workspace package version`);
    }
  }
}

// Package all members at once so Cargo can resolve unpublished dependencies.
// Build the normalized archives below rather than the checkout's path sources.
run("cargo", [
  "package", "--workspace", ...excluded.flatMap((pkg) => ["--exclude", pkg.name]),
  "--locked", "--no-verify", ...options,
]);

const directory = mkdtempSync(join(tmpdir(), "ctl-cargo-packages-"));
try {
  const members = packages.map((pkg) => `${pkg.name}-${pkg.version}`);
  for (const [index, member] of members.entries()) {
    run("tar", ["-xzf", join(metadata.target_directory, "package", `${member}.crate`), "-C", directory]);
    if (readFileSync(join(directory, member, "LICENSE"), "utf8") !== package_licenses.get(packages[index].name)) {
      throw new Error(`${member}: archive license differs from its source package`);
    }
  }
  const patches = packages.map((pkg, index) => `${pkg.name} = { path = ${JSON.stringify(members[index])} }`);
  writeFileSync(join(directory, "Cargo.toml"), [
    "[workspace]", 'resolver = "3"', `members = ${JSON.stringify(members)}`,
    "", "[patch.crates-io]", ...patches, "",
  ].join("\n"));
  // Retain the checkout's external dependency versions while allowing Cargo to
  // remove the excluded desktop package from this isolated validation workspace.
  copyFileSync(join(root, "Cargo.lock"), join(directory, "Cargo.lock"));
  const target = join(metadata.target_directory, "publish-check");
  // Cargo archives normalize source timestamps, so a new extraction can reuse
  // stale build-script outputs from the previous verification. Rebuild every
  // packaged member while retaining the external dependency cache.
  run("cargo", [
    "clean", "--target-dir", target, ...packages.flatMap((pkg) => ["--package", pkg.name]), ...offline,
  ], directory);
  run("cargo", ["test", "--workspace", "--all-features", "--target-dir", target, ...offline], directory);
  console.log(`Verified archives and tests for ${packages.length} crates.`);
} finally {
  rmSync(directory, { recursive: true, force: true });
}
