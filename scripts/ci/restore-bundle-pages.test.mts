import assert from "node:assert/strict";
import { cp, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { componentSetFixture, publicationFixture } from "./bundle-publication.fixtures.mts";
import { bundleRoot, validatedComponentSet } from "./bundle-publication.mts";
import { pagesDeployStep, pagesJobName, restoreBundlePages, successfulSnapshot } from "./restore-bundle-pages.mts";
import { parseBundleCatalog, stageBundlePages } from "./stage-bundle-pages.mts";
import type { GhRunner } from "./update-draft-release.mts";

async function root(t: TestContext) {
  const path = await mkdtemp(join(tmpdir(), "ctl-restore-pages-test-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}

function github(options: { artifact_site?: string; deployed?: boolean; legacy?: boolean; html_url?: string; statuses?: string[][] } = {}): GhRunner {
  return async (args) => {
    const endpoint = args[1]!;
    if (args[0] === "run") {
      assert.ok(options.artifact_site);
      await cp(options.artifact_site, args[args.indexOf("--dir") + 1]!, { recursive: true });
      return "";
    }
    if (endpoint.endsWith("/pages")) return JSON.stringify({ build_type: options.legacy ? "legacy" : "workflow", html_url: options.html_url ?? new URL("../", bundleRoot).href, cname: null });
    if (endpoint.includes("/actions/artifacts")) return JSON.stringify([{ artifacts: options.artifact_site ? [{ id: 1, name: "bundle-pages-state-1001-1", expired: false, created_at: "2026-01-01T00:00:00Z", workflow_run: { id: 1001 } }] : [] }]);
    if (endpoint.endsWith("/actions/runs/1001")) return JSON.stringify({ event: "push", path: ".github/workflows/bundles.yml", head_repository: { full_name: "tokn-ai/ctl" }, conclusion: "failure" });
    if (endpoint.includes("/attempts/1/jobs")) return JSON.stringify([{ jobs: [{ name: pagesJobName, steps: [{ name: pagesDeployStep, conclusion: "success" }] }] }]);
    const statuses = options.statuses ?? (options.deployed ? [["success"]] : []);
    if (endpoint.includes("/deployments?")) return JSON.stringify([statuses.map((_, index) => ({ id: index + 1 }))]);
    const deployment = /\/deployments\/(\d+)\/statuses\?/.exec(endpoint);
    if (deployment) return JSON.stringify([statuses[Number(deployment[1]) - 1]!.map((state) => ({ state }))]);
    throw new Error(`Unexpected call: ${args.join(" ")}`);
  };
}

function publicSite(path: string | null): typeof fetch {
  return (async (input: string | URL | Request) => {
    const url = new URL(String(input));
    assert.equal(url.origin, "https://tokn-ai.dev");
    assert.ok(url.pathname.startsWith("/ctl/"));
    if (!path) return new Response("missing", { status: 404 });
    try { return new Response(await readFile(join(path, url.pathname.slice("/ctl/".length)))); }
    catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return new Response("missing", { status: 404 }); throw error; }
  }) as typeof fetch;
}

test("snapshot selection requires the exact successful deployment attempt, not a successful desktop/workflow", async () => {
  const calls: string[] = [];
  const selected = await successfulSnapshot("tokn-ai/ctl", async (args) => {
    const endpoint = args[1]!;
    calls.push(endpoint);
    if (endpoint.includes("/artifacts")) return JSON.stringify([{ artifacts: [2, 1].map((attempt) => ({ id: attempt, name: `bundle-pages-state-42-${attempt}`, expired: false, created_at: `2026-01-0${attempt}T00:00:00Z`, workflow_run: { id: 42 } })) }]);
    if (endpoint.endsWith("/runs/42")) return JSON.stringify({ event: "push", path: ".github/workflows/bundles.yml", head_repository: { full_name: "tokn-ai/ctl" }, conclusion: "failure" });
    return JSON.stringify([{ jobs: [{ name: pagesJobName, steps: [{ name: pagesDeployStep, conclusion: endpoint.includes("attempts/1/") ? "success" : "failure" }] }] }]);
  });
  assert.equal(selected?.name, "bundle-pages-state-42-1");
  assert.ok(calls.some((endpoint) => endpoint.includes("attempts/2/jobs")));
  assert.ok(calls.some((endpoint) => endpoint.includes("attempts/1/jobs")));
});

test("restores a deployed snapshot even while Pages CDN is still missing its catalog", async (t) => {
  const directory = await root(t), entry = publicationFixture(1), site = join(directory, "site"), output = join(directory, "restore");
  await stageBundlePages(entry, await componentSetFixture(directory, entry), null, site);
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github({ artifact_site: site }), publicSite(null)), true);
  await validatedComponentSet(join(output, "bundles/sets", entry.publication_id), entry);
});

test("expired snapshots recover actual archives and both channel pointers from the managed HTTPS site", async (t) => {
  const directory = await root(t), first = publicationFixture(1), second = publicationFixture(2, "stable");
  const old = join(directory, "old"), site = join(directory, "site"), output = join(directory, "restore");
  await stageBundlePages(first, await componentSetFixture(directory, first), null, old);
  await stageBundlePages(second, await componentSetFixture(directory, second), old, site);
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github({ deployed: true }), publicSite(site)), true);
  assert.deepEqual(parseBundleCatalog(await readFile(join(output, "bundles/catalog.json"))).channels,
    parseBundleCatalog(await readFile(join(site, "bundles/catalog.json"))).channels);
  for (const entry of [first, second]) await validatedComponentSet(join(output, "bundles/sets", entry.publication_id), entry);
});

test("a newer published generation wins over an older surviving Actions snapshot", async (t) => {
  const directory = await root(t), first = publicationFixture(1), second = publicationFixture(2, "stable");
  const old = join(directory, "old"), site = join(directory, "site"), output = join(directory, "restore");
  await stageBundlePages(first, await componentSetFixture(directory, first), null, old);
  await stageBundlePages(second, await componentSetFixture(directory, second), old, site);
  await restoreBundlePages("tokn-ai/ctl", output, github({ artifact_site: old }), publicSite(site));
  assert.equal(parseBundleCatalog(await readFile(join(output, "bundles/catalog.json"))).generation, 2);
});

test("equal-generation divergent catalogs fail closed instead of discarding a channel", async (t) => {
  const directory = await root(t), dev = publicationFixture(1), stable = publicationFixture(2, "stable");
  const artifact = join(directory, "artifact"), site = join(directory, "site");
  await stageBundlePages(dev, await componentSetFixture(directory, dev), null, artifact);
  await stageBundlePages(stable, await componentSetFixture(directory, stable), null, site);
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", join(directory, "restore"), github({ artifact_site: artifact }), publicSite(site)), /conflicting content/);
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", join(directory, "wrong-attempt"), github({ artifact_site: site }), publicSite(null)), /does not belong/);
});

test("first deployment ignores current pending and failed jobs but preserves successful deployment history", async (t) => {
  const directory = await root(t), output = join(directory, "restore");
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github(), publicSite(null)), false);
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github({ statuses: [["in_progress", "queued"], ["failure"], ["error", "pending"]] }), publicSite(null)), false);
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github({ statuses: [["in_progress"], ["failure", "success"]] }), publicSite(null)), /no managed catalog/);
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github({ statuses: [["inactive"]] }), publicSite(null)), /no managed catalog/);
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github({ deployed: true }), publicSite(null)), /no managed catalog/);
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github({ legacy: true }), publicSite(null)), /another publishing source/);
  const manual: typeof fetch = (async (input: string | URL | Request) => new Response("manual website", { status: String(input).includes("catalog.json") ? 404 : 200 })) as typeof fetch;
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github(), manual), /no managed catalog/);
});

test("Actions waiting status permits first publication but never erases earlier publication history", async (t) => {
  const directory = await root(t), output = join(directory, "restore");
  // Observed on the initial Pages job: failure, in_progress, queued, waiting.
  const statuses = [["failure", "in_progress", "queued", "waiting"]];
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github({ statuses }), publicSite(null)), false);
  for (const state of ["success", "inactive"]) {
    await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github({ statuses: [...statuses, [state]] }), publicSite(null)), /no managed catalog/);
  }
});

test("unknown or malformed deployment states fail closed with the deployment and invalid value", async (t) => {
  const directory = await root(t), output = join(directory, "restore");
  for (const status of [null, {}, { state: null }, { state: 42 }, { state: "future_status" }]) {
    const underlying = github({ statuses: [["waiting"]] });
    const gh: GhRunner = async (args) => args[1]?.includes("/statuses?") ? JSON.stringify([[status]]) : underlying(args);
    const invalid = status?.state;
    await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, gh, publicSite(null)), (error: unknown) => {
      assert.ok(error instanceof Error);
      assert.equal(error.message, `Invalid Pages deployment status for deployment 1: ${JSON.stringify(invalid)}`);
      return true;
    });
    await assert.rejects(readFile(join(output, "bundles/catalog.json")), { code: "ENOENT" });
  }
});

test("Pages configuration accepts only the canonical domain and path with HTTPS normalization", async (t) => {
  const directory = await root(t), output = join(directory, "restore");
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github({ html_url: "http://tokn-ai.dev/ctl/" }), publicSite(null)), false);
  for (const html_url of ["https://tokn-ai.github.io/ctl/", "https://other.example/ctl/", "https://tokn-ai.dev/elsewhere/", "https://user@tokn-ai.dev/ctl/", "https://tokn-ai.dev/ctl/?query=1", "https://tokn-ai.dev/ctl/#fragment", "ftp://tokn-ai.dev/ctl/", "invalid"]) {
    await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github({ html_url }), publicSite(null)), /another publishing source/);
  }
});

test("HTTP errors or malformed published catalogs never initialize an empty site", async (t) => {
  const directory = await root(t), output = join(directory, "restore");
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github(), async () => new Response("offline", { status: 503 })), /HTTP 503/);
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github(), async () => new Response("{}")), /not a supported managed bundle site/);
  await assert.rejects(readFile(join(output, "bundles/catalog.json")), { code: "ENOENT" });
});

test("workflow publication depends on components, not desktop success or the latest unpublished main commit", async () => {
  const workflow = await readFile(new URL("../../.github/workflows/bundles.yml", import.meta.url), "utf8");
  const pages = workflow.split("  bundle-pages:")[1]!.split("  ctld:")[0]!;
  assert.ok(pages.includes("needs: [metadata, remote-agent-set, component-release]"));
  assert.ok(!pages.includes("needs.desktop"));
  assert.ok(!pages.includes("git/ref/heads/main"));
  assert.ok(pages.includes("queue: max"));
  assert.ok(pages.includes(`name: ${pagesDeployStep}`));
  assert.ok(workflow.includes("CTL_BUNDLE_CHANNEL:"));
  assert.ok(workflow.includes("publish-component-release.mts desktop"));
});
