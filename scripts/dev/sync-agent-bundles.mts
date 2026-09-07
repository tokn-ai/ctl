import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const WORKFLOW = "bundles.yml";
const ARTIFACT = "ctl-agent-bundle-set";
const BUNDLE_SET_FILE = "bundle-set.json";
const SUPPORTED_TARGETS = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
] as const;

interface WorkflowRun {
  conclusion: string;
  createdAt: string;
  databaseId: number;
  event: string;
  headSha: string;
  status: string;
}

interface BundleTarget {
  archive: string;
  sha256: string;
}

interface BundleSet {
  schema_version: number;
  app_version: string;
  bundle_id: string;
  git_revision: string;
  targets: Record<string, BundleTarget>;
}

function run(
  command: string,
  args: string[],
  cwd: string,
  inherit = false,
): string {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    stdio: inherit ? "inherit" : "pipe",
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    const detail = inherit ? "" : (result.stderr || result.stdout).trim();
    throw new Error(
      `${command} ${args.join(" ")} failed${detail ? `: ${detail}` : ""}`,
    );
  }
  return inherit ? "" : result.stdout.trim();
}

function git(args: string[], cwd: string): string {
  return run("git", args, cwd);
}

function gh(args: string[], cwd: string, inherit = false): string {
  return run("gh", args, cwd, inherit);
}

function parseRuns(json: string): WorkflowRun[] {
  const value: unknown = JSON.parse(json);
  if (!Array.isArray(value)) {
    throw new Error("GitHub returned an invalid workflow-run list");
  }
  return value as WorkflowRun[];
}

function listRuns(repoRoot: string, filters: string[]): WorkflowRun[] {
  return parseRuns(
    gh(
      [
        "run",
        "list",
        "--workflow",
        WORKFLOW,
        ...filters,
        "--limit",
        "20",
        "--json",
        "databaseId,status,conclusion,headSha,event,createdAt",
      ],
      repoRoot,
    ),
  );
}

function isActive(run: WorkflowRun): boolean {
  return run.status !== "completed";
}

function hasFreshArtifacts(run: WorkflowRun): boolean {
  const age = Date.now() - Date.parse(run.createdAt);
  return Number.isFinite(age) && age < 29 * 24 * 60 * 60 * 1_000;
}

async function dispatchRun(
  repoRoot: string,
  branch: string,
  revision: string,
  previousRunIds: Set<number>,
): Promise<WorkflowRun> {
  const output = gh(
    ["workflow", "run", WORKFLOW, "--ref", branch],
    repoRoot,
  );
  const runId = output.match(/\/actions\/runs\/(\d+)/)?.[1];
  if (runId) {
    return {
      conclusion: "",
      createdAt: new Date().toISOString(),
      databaseId: Number(runId),
      event: "workflow_dispatch",
      headSha: revision,
      status: "queued",
    };
  }

  for (let attempt = 0; attempt < 30; attempt += 1) {
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 2_000));
    const run = listRuns(repoRoot, ["--commit", revision]).find(
      (candidate) =>
        candidate.event === "workflow_dispatch" &&
        !previousRunIds.has(candidate.databaseId),
    );
    if (run) {
      return run;
    }
  }
  throw new Error("The dispatched bundle workflow did not appear within one minute");
}

function asRecord(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${name} must be a JSON object`);
  }
  return value as Record<string, unknown>;
}

function requireString(
  record: Record<string, unknown>,
  field: string,
): string {
  const value = record[field];
  if (typeof value !== "string" || !value) {
    throw new Error(`bundle-set field ${field} must be a non-empty string`);
  }
  return value;
}

function readBundleSet(
  directory: string,
  appVersion: string,
  expectedRevision: string | undefined,
  verifyArchives: boolean,
): BundleSet {
  const manifestPath = join(directory, BUNDLE_SET_FILE);
  const root = asRecord(JSON.parse(readFileSync(manifestPath, "utf8")), "bundle set");
  const schemaVersion = root.schema_version;
  const manifestAppVersion = requireString(root, "app_version");
  const bundleId = requireString(root, "bundle_id");
  const gitRevision = requireString(root, "git_revision");
  const targets = asRecord(root.targets, "bundle-set targets");

  if (schemaVersion !== 1) {
    throw new Error(`unsupported bundle-set schema version: ${String(schemaVersion)}`);
  }
  if (manifestAppVersion !== appVersion) {
    throw new Error(
      `bundle set targets app ${manifestAppVersion}, but this checkout is ${appVersion}`,
    );
  }
  if (!/^[a-zA-Z0-9._+-]{1,128}$/.test(bundleId)) {
    throw new Error(`invalid bundle id: ${bundleId}`);
  }
  if (!/^[0-9a-fA-F]{40}$/.test(gitRevision)) {
    throw new Error(`invalid bundle git revision: ${gitRevision}`);
  }
  const developmentBundleId = `${manifestAppVersion}-dev.${gitRevision.slice(0, 12)}`;
  if (bundleId !== manifestAppVersion && bundleId !== developmentBundleId) {
    throw new Error("bundle id does not match its app version and Git revision");
  }
  if (expectedRevision && gitRevision !== expectedRevision) {
    throw new Error(
      `bundle set is for ${gitRevision.slice(0, 12)}, not ${expectedRevision.slice(0, 12)}`,
    );
  }
  const targetNames = Object.keys(targets).sort();
  if (
    targetNames.length !== SUPPORTED_TARGETS.length ||
    SUPPORTED_TARGETS.some((target) => !targetNames.includes(target))
  ) {
    throw new Error("bundle set does not contain every supported target");
  }

  const parsedTargets: Record<string, BundleTarget> = {};
  for (const target of SUPPORTED_TARGETS) {
    const entry = asRecord(targets[target], `bundle target ${target}`);
    const archive = requireString(entry, "archive");
    const sha256 = requireString(entry, "sha256").toLowerCase();
    const expectedArchive = `ctl-agent-bundle-${bundleId}-${target}.tar.gz`;
    if (archive !== expectedArchive || !/^[0-9a-f]{64}$/.test(sha256)) {
      throw new Error(`invalid bundle metadata for ${target}`);
    }
    if (verifyArchives) {
      const archivePath = join(directory, archive);
      const actual = createHash("sha256")
        .update(readFileSync(archivePath))
        .digest("hex");
      if (actual !== sha256) {
        throw new Error(`checksum mismatch for ${archive}`);
      }
      const sidecar = readFileSync(`${archivePath}.sha256`, "utf8")
        .trim()
        .split(/\s+/u);
      if (sidecar[0]?.toLowerCase() !== sha256 || sidecar[1] !== archive) {
        throw new Error(`invalid checksum sidecar for ${archive}`);
      }
    }
    parsedTargets[target] = { archive, sha256 };
  }

  return {
    schema_version: 1,
    app_version: manifestAppVersion,
    bundle_id: bundleId,
    git_revision: gitRevision,
    targets: parsedTargets,
  };
}

function appVersion(repoRoot: string): string {
  const configPath = join(repoRoot, "apps/rmux/src-tauri/tauri.conf.json");
  const config = asRecord(JSON.parse(readFileSync(configPath, "utf8")), "Tauri config");
  return requireString(config, "version");
}

function checkBundles(repoRoot: string): void {
  const destination = join(
    repoRoot,
    "apps/rmux/src-tauri/resources/agent-bundles",
  );
  const revision = git(["rev-parse", "HEAD"], repoRoot);
  if (!existsSync(join(destination, BUNDLE_SET_FILE))) {
    console.warn("Remote install bundles have not been synchronized for development.");
    console.warn("Run `pnpm agents:sync` from apps/rmux when testing remote installation.");
    return;
  }
  try {
    readBundleSet(destination, appVersion(repoRoot), revision, true);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    console.warn(`Remote install bundles are not current: ${detail}`);
    console.warn("Run `pnpm agents:sync` from apps/rmux when testing remote installation.");
  }
}

function installBundleSet(source: string, destination: string, manifest: BundleSet): void {
  mkdirSync(destination, { recursive: true });
  for (const target of SUPPORTED_TARGETS) {
    const archive = manifest.targets[target].archive;
    copyFileSync(join(source, archive), join(destination, archive));
    copyFileSync(join(source, `${archive}.sha256`), join(destination, `${archive}.sha256`));
  }
  const temporaryManifest = join(
    destination,
    `.bundle-set-${process.pid}.json`,
  );
  copyFileSync(join(source, BUNDLE_SET_FILE), temporaryManifest);
  renameSync(temporaryManifest, join(destination, BUNDLE_SET_FILE));
}

async function syncBundles(repoRoot: string, useMain: boolean): Promise<void> {
  const version = appVersion(repoRoot);
  let revision: string;
  let run: WorkflowRun | undefined;

  if (useMain) {
    run = listRuns(repoRoot, ["--branch", "main", "--event", "push", "--status", "success"])
      .find(hasFreshArtifacts);
    if (!run) {
      throw new Error("No successful main bundle workflow is available");
    }
    revision = run.headSha;
  } else {
    if (git(["status", "--porcelain"], repoRoot)) {
      throw new Error("Commit or stash local changes before syncing an exact bundle set");
    }
    revision = git(["rev-parse", "HEAD"], repoRoot);
    const branch = git(["branch", "--show-current"], repoRoot);
    if (!branch) {
      throw new Error("Check out a pushed branch before syncing an exact bundle set");
    }
    let upstreamRevision: string;
    try {
      upstreamRevision = git(["rev-parse", "@{upstream}"], repoRoot);
    } catch {
      throw new Error(`Branch ${branch} has no upstream; push it before syncing bundles`);
    }
    if (upstreamRevision !== revision) {
      throw new Error(`Push ${branch} before syncing bundles for its current commit`);
    }

    const existingRuns = listRuns(repoRoot, ["--commit", revision]);
    run = existingRuns.find(
      (candidate) => candidate.conclusion === "success" && hasFreshArtifacts(candidate),
    )
      ?? existingRuns.find(isActive);
    if (!run) {
      console.log(`No bundle run exists for ${revision.slice(0, 12)}; dispatching CI…`);
      run = await dispatchRun(
        repoRoot,
        branch,
        revision,
        new Set(existingRuns.map((candidate) => candidate.databaseId)),
      );
    }
  }

  if (isActive(run)) {
    console.log(`Waiting for bundle workflow run ${run.databaseId}…`);
    gh(
      ["run", "watch", String(run.databaseId), "--compact", "--exit-status"],
      repoRoot,
      true,
    );
  }
  const completed = JSON.parse(
    gh(
      [
        "run",
        "view",
        String(run.databaseId),
        "--json",
        "conclusion,headSha,status",
      ],
      repoRoot,
    ),
  ) as { conclusion: string; headSha: string; status: string };
  if (
    completed.status !== "completed" ||
    completed.conclusion !== "success" ||
    completed.headSha !== revision
  ) {
    throw new Error(`Bundle workflow run ${run.databaseId} did not complete successfully`);
  }

  const temporaryDirectory = mkdtempSync(join(tmpdir(), "ctl-agent-bundles-"));
  try {
    gh(
      [
        "run",
        "download",
        String(run.databaseId),
        "--name",
        ARTIFACT,
        "--dir",
        temporaryDirectory,
      ],
      repoRoot,
      true,
    );
    const manifest = readBundleSet(temporaryDirectory, version, revision, true);
    const destination = join(
      repoRoot,
      "apps/rmux/src-tauri/resources/agent-bundles",
    );
    installBundleSet(temporaryDirectory, destination, manifest);
    console.log(
      `Installed ${manifest.bundle_id} from run ${run.databaseId} into ${resolve(destination)}`,
    );
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.length > 1 || (args[0] && !["--check", "--main", "--help"].includes(args[0]))) {
    throw new Error("usage: pnpm agents:sync [--main]");
  }
  if (args[0] === "--help") {
    console.log("usage: pnpm agents:sync [--main]");
    console.log("  default  sync or build bundles for the exact pushed commit");
    console.log("  --main   use the latest successful main-branch bundle set");
    return;
  }

  const repoRoot = git(["rev-parse", "--show-toplevel"], process.cwd());
  if (args[0] === "--check") {
    checkBundles(repoRoot);
    return;
  }
  await syncBundles(repoRoot, args[0] === "--main");
}

main().catch((error: unknown) => {
  const detail = error instanceof Error ? error.message : String(error);
  console.error(`agent bundle sync failed: ${detail}`);
  process.exitCode = 1;
});
