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
import { agentTargets, maxAgentManifestBytes, parseAgentBundleSet, readAgentFile, verifyAgentBundleTarget, type AgentBundleSet } from "../shared/agent-bundle.mts";

const WORKFLOW = "bundles.yml";
const ARTIFACT = "ctl-agent-bundle-set";
const BUNDLE_SET_FILE = "bundle-set.json";
const SUPPORTED_TARGETS = agentTargets;

interface WorkflowRun {
  conclusion: string;
  createdAt: string;
  databaseId: number;
  event: string;
  headSha: string;
  status: string;
}

type BundleSet = AgentBundleSet;

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
    ["workflow", "run", WORKFLOW, "--ref", branch, "-f", "build_desktop=false"],
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

async function readBundleSet(
  directory: string,
  appVersion: string,
  expectedRevision: string | undefined,
  verifyArchives: boolean,
): Promise<BundleSet> {
  const manifest = parseAgentBundleSet(
    await readAgentFile(join(directory, BUNDLE_SET_FILE), maxAgentManifestBytes),
    { app_version: appVersion, ...(expectedRevision ? { git_revision: expectedRevision } : {}) },
  );
  if (verifyArchives) {
    for (const target of SUPPORTED_TARGETS) {
      await verifyAgentBundleTarget(directory, manifest, target, manifest.targets[target]);
    }
  }
  return manifest;
}

function appVersion(repoRoot: string): string {
  const configPath = join(repoRoot, "apps/desktop/src-tauri/tauri.conf.json");
  const config = asRecord(JSON.parse(readFileSync(configPath, "utf8")), "Tauri config");
  return requireString(config, "version");
}

async function checkBundles(repoRoot: string): Promise<void> {
  const destination = join(
    repoRoot,
    "apps/desktop/src-tauri/resources/agent-bundles",
  );
  const revision = git(["rev-parse", "HEAD"], repoRoot);
  if (!existsSync(join(destination, BUNDLE_SET_FILE))) {
    console.warn("Remote install bundles have not been synchronized for development.");
    console.warn("Run `pnpm agents:sync` from apps/desktop when testing remote installation.");
    return;
  }
  try {
    await readBundleSet(destination, appVersion(repoRoot), revision, true);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    console.warn(`Remote install bundles are not current: ${detail}`);
    console.warn("Run `pnpm agents:sync` from apps/desktop when testing remote installation.");
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
    const manifest = await readBundleSet(temporaryDirectory, version, revision, true);
    const destination = join(
      repoRoot,
      "apps/desktop/src-tauri/resources/agent-bundles",
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
    await checkBundles(repoRoot);
    return;
  }
  await syncBundles(repoRoot, args[0] === "--main");
}

main().catch((error: unknown) => {
  const detail = error instanceof Error ? error.message : String(error);
  console.error(`agent bundle sync failed: ${detail}`);
  process.exitCode = 1;
});
