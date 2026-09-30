import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import type { DaemonExecutables } from "./daemon-build.mts";
import { withAppRunner } from "./signed-cargo.mts";
import { metadataArguments, targetFromArtifacts } from "./signed-cargo-target.mts";

const metadataPrefix = ["metadata", "--no-deps", "--format-version", "1"];

test("metadata retains Cargo resolution settings and excludes app/build-only options", () => {
  assert.deepEqual(metadataArguments([
    "run", "--manifest-path", "workspace with spaces/Cargo.toml",
    "--config", "config with spaces.toml", "--config=build.incremental=false",
    "--target", "targets/custom-macos.json", "--profile", "development",
    "--target-dir", "custom output", "--package", "rmux-app",
    "--bin=rmux-app", "--features", "devtools", "--no-default-features",
    "--jobs", "4", "--color", "always", "--release", "--locked", "--offline",
    "--", "--manifest-path", "app argument", "--config", "app config",
  ]), [
    ...metadataPrefix,
    "--manifest-path", "workspace with spaces/Cargo.toml",
    "--config", "config with spaces.toml", "--config", "build.incremental=false",
    "--locked", "--offline", "--config", 'build.target-dir="custom output"',
  ]);
});

test("metadata target-dir override follows user config and preserves its exact value", () => {
  const targetDirectory = 'custom output/with "quotes" and = signs';
  const args = metadataArguments([
    "run", "--target-dir=earlier output", "--config", 'build.target-dir="configured output"',
    `--target-dir=${targetDirectory}`, "--config", "override.toml",
    "--manifest-path=workspace=one/Cargo.toml", "--frozen",
  ]);
  assert.deepEqual(args, [
    ...metadataPrefix,
    "--config", 'build.target-dir="configured output"', "--config", "override.toml",
    "--manifest-path", "workspace=one/Cargo.toml", "--frozen",
    "--config", `build.target-dir=${JSON.stringify(targetDirectory)}`,
  ]);
  assert.equal(JSON.parse(args.at(-1)!.slice("build.target-dir=".length)), targetDirectory);
});

test("metadata forwards unstable Cargo switches without forwarding profile or target selectors", () => {
  assert.deepEqual(metadataArguments([
    "run", "-Z", "unstable-options", "-Zscript",
    "--target=custom-target", "--profile=fast-check", "-j2", "-vv",
    "--artifact-dir", "artifacts", "--message-format=json", "--timings",
    "--", "-Z", "app-only",
  ]), [...metadataPrefix, "-Z", "unstable-options", "-Zscript"]);
  assert.deepEqual(metadataArguments(["run", "--", "--target-dir=app-only"]), metadataPrefix);
  assert.throws(() => metadataArguments(["run", "--target-dir"]), /requires a value/);
});

function artifacts(directory: string): DaemonExecutables {
  return {
    ctld: path.join(directory, "ctld"),
    rmuxd: path.join(directory, "rmuxd"),
    taskd: path.join(directory, "taskd"),
  };
}

test("host artifacts leave target selection to the compiler for every profile", () => {
  const root = path.resolve("fixture output");
  for (const profile of ["debug", "release", "development"]) {
    assert.equal(targetFromArtifacts(root, artifacts(path.join(root, profile))), undefined);
  }
});

test("target artifacts identify triples and custom JSON target names across profiles", () => {
  const root = path.resolve("custom output");
  for (const [target, profile] of [
    ["aarch64-apple-darwin", "debug"],
    ["x86_64-apple-darwin", "release"],
    ["custom-macos", "development"],
  ] as const) {
    assert.equal(targetFromArtifacts(root, artifacts(path.join(root, target, profile))), target);
  }
});

test("rejects helpers from different targets or profiles", () => {
  const root = path.resolve("fixture output");
  const common = artifacts(path.join(root, "aarch64-apple-darwin", "debug"));
  for (const different of [
    path.join(root, "aarch64-apple-darwin", "release", "rmuxd"),
    path.join(root, "x86_64-apple-darwin", "debug", "rmuxd"),
  ]) {
    assert.throws(() => targetFromArtifacts(root, { ...common, rmuxd: different }), /one Cargo target\/profile/);
  }
});

test("rejects artifacts outside Cargo output or without a valid target/profile layout", () => {
  const root = path.resolve("fixture output");
  for (const directory of [
    root,
    path.dirname(root),
    `${root}-sibling${path.sep}debug`,
    path.join(root, "target", "debug", "deps"),
  ]) {
    assert.throws(() => targetFromArtifacts(root, artifacts(directory)), /outside Cargo's reported target directory/);
  }
});

test("app runner is the final Cargo override while preserving all original arguments", () => {
  const args = [
    "run", "--target", "custom-target", "--profile=development",
    "--target-dir", "output with spaces", "--features", "devtools,fixture",
    "--config", 'target.custom-target.runner=["previous runner"]',
    "--", "app argument with spaces", "", "--config", "literal app config", "--",
  ];
  const original = [...args];
  const separator = args.indexOf("--");
  const result = withAppRunner(args, "custom-target");
  assert.deepEqual(args, original);
  assert.deepEqual(result.slice(0, separator), original.slice(0, separator));
  assert.deepEqual(result.slice(separator + 2), original.slice(separator));
  assert.equal(result[separator], "--config");
  const override = result[separator + 1]!;
  const prefix = 'target."custom-target".runner=';
  assert.ok(override.startsWith(prefix));
  assert.deepEqual(JSON.parse(override.slice(prefix.length)), [
    process.execPath, "--experimental-strip-types", "--disable-warning=ExperimentalWarning",
    fileURLToPath(new URL("./signed-app-relay.mts", import.meta.url)),
  ]);
});

test("app runner appends its override when Cargo has no app argument separator", () => {
  const args = ["run", "--config=target.fixture.runner='previous runner'", "--locked"];
  const result = withAppRunner(args, "fixture");
  assert.deepEqual(result.slice(0, args.length), args);
  assert.equal(result.length, args.length + 2);
  assert.equal(result.at(-2), "--config");
  assert.match(result.at(-1)!, /^target\."fixture"\.runner=/);
});
