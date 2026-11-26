import assert from "node:assert/strict";
import { cp, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { archiveName } from "../shared/agent-bundle.mts";
import { componentSetFixture, legacyNightlyFixture, legacyPagesCatalog, publicationFixture } from "./bundle-publication.fixtures.mts";
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

test("restores a deployed legacy snapshot with both pointers while Pages CDN is still missing its catalog", async (t) => {
  const directory = await root(t), entry = legacyNightlyFixture(1), site = join(directory, "site"), output = join(directory, "restore");
  await stageBundlePages(entry, await componentSetFixture(directory, entry), null, site);
  await legacyPagesCatalog(site, true);
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github({ artifact_site: site }), publicSite(null)), true);
  const catalog = parseBundleCatalog(await readFile(join(output, "bundles/catalog.json")));
  assert.deepEqual(catalog.channels, { nightly: entry.publication_id, dev: entry.publication_id });
  assert.equal(catalog.publications[0].channel, "dev");
  await validatedComponentSet(join(output, "bundles/sets", entry.publication_id), entry);
});

test("snapshot-only recovery restores both published channels without consulting the Pages site", async (t) => {
  const directory = await root(t), nightly = publicationFixture(1), stable = publicationFixture(2, "stable");
  const previous = join(directory, "previous"), site = join(directory, "site"), output = join(directory, "restore");
  await stageBundlePages(stable, await componentSetFixture(directory, stable), null, previous);
  await stageBundlePages(nightly, await componentSetFixture(directory, nightly), previous, site);
  const broken_site: typeof fetch = async () => new Response("offline", { status: 503 });
  const gh = github({ artifact_site: site });
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, gh, broken_site), /HTTP 503/);
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, gh, broken_site, true), true);
  const restored = parseBundleCatalog(await readFile(join(output, "bundles/catalog.json")));
  assert.deepEqual(restored.channels, { nightly: nightly.publication_id, stable: stable.publication_id });
  for (const entry of [nightly, stable]) {
    const archive = join("bundles/sets", entry.publication_id, archiveName(entry, "aarch64-apple-darwin"));
    assert.deepEqual(await readFile(join(output, archive)), await readFile(join(site, archive)));
  }
});

test("snapshot-only recovery rejects missing or damaged bundle snapshots", async (t) => {
  const directory = await root(t), entry = publicationFixture(1);
  const site = join(directory, "site"), output = join(directory, "restore");
  const unused: typeof fetch = async () => assert.fail("snapshot-only recovery fetched the Pages site");
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, github(), unused, true), /No successful bundle Pages snapshot/);

  await stageBundlePages(entry, await componentSetFixture(directory, entry), null, site);
  const gh = github({ artifact_site: site });
  const pointer = join(site, "bundles/channels/nightly.json");
  const original = await readFile(pointer);
  await writeFile(pointer, original.toString().replace(entry.publication_id, "different"));
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, gh, unused, true), /channel and catalog disagree/);
  await writeFile(pointer, original);

  const foreign = join(site, "unexpected.html");
  await writeFile(foreign, "unknown file");
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, gh, unused, true), /non-bundle Pages content/);
  await rm(foreign);

  await rm(join(site, "bundles/sets", entry.publication_id, archiveName(entry, "aarch64-apple-darwin")));
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", output, gh, unused, true), { code: "ENOENT" });
  await assert.rejects(readFile(join(output, "bundles/catalog.json")), { code: "ENOENT" });
});

test("expired snapshots recover actual archives and all channel pointers from the managed HTTPS site", async (t) => {
  const directory = await root(t), first = publicationFixture(1), second = publicationFixture(2, "stable");
  const old = join(directory, "old"), site = join(directory, "site"), output = join(directory, "restore");
  await stageBundlePages(first, await componentSetFixture(directory, first, true), null, old);
  await stageBundlePages(second, await componentSetFixture(directory, second), old, site);
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github({ deployed: true }), publicSite(site)), true);
  assert.deepEqual(parseBundleCatalog(await readFile(join(output, "bundles/catalog.json"))).channels,
    parseBundleCatalog(await readFile(join(site, "bundles/catalog.json"))).channels);
  for (const channel of ["nightly", "stable"]) {
    const relative = `bundles/channels/${channel}.json`;
    assert.deepEqual(await readFile(join(output, relative)), await readFile(join(site, relative)));
  }
  for (const entry of [first, second]) await validatedComponentSet(join(output, "bundles/sets", entry.publication_id), entry);
  assert.deepEqual(await readFile(join(output, "bundles/sets", first.publication_id, "build-info.json")),
    await readFile(join(site, "bundles/sets", first.publication_id, "build-info.json")));
  await writeFile(join(site, "bundles/channels/nightly.json"), await readFile(join(site, "bundles/channels/stable.json")));
  await assert.rejects(restoreBundlePages("tokn-ai/ctl", join(directory, "corrupt"), github(), publicSite(site)), /channel and catalog disagree/);
});

test("restores a legacy HTTPS catalog without requiring a Nightly pointer", async (t) => {
  const directory = await root(t), dev = legacyNightlyFixture(1);
  const site = join(directory, "legacy"), output = join(directory, "restore");
  await stageBundlePages(dev, await componentSetFixture(directory, dev), null, site);
  await legacyPagesCatalog(site, false);
  assert.equal(await restoreBundlePages("tokn-ai/ctl", output, github({ deployed: true }), publicSite(site)), true);
  const catalog = parseBundleCatalog(await readFile(join(output, "bundles/catalog.json")));
  assert.deepEqual(catalog.channels, { dev: dev.publication_id });
  assert.equal(catalog.publications[0].channel, "dev");
  await assert.rejects(readFile(join(output, "bundles/channels/nightly.json")), { code: "ENOENT" });
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

test("workflow advances Nightly from main component builds and keeps PR builds in Actions", async () => {
  const workflow = await readFile(new URL("../../.github/workflows/bundles.yml", import.meta.url), "utf8");
  const pages = workflow.split("  bundle-pages:")[1]!.split("  ctld:")[0]!;
  assert.ok(pages.includes("needs: [metadata, remote-agent-set, remote-agent-windows, component-release]"));
  assert.ok(!pages.includes("needs.desktop"));
  assert.ok(!pages.includes("needs.remote-agent-windows.result"));
  assert.ok(!pages.includes("git/ref/heads/main"));
  assert.match(pages, /github\.ref == 'refs\/heads\/main' \|\|\s+\(startsWith\(github\.ref, 'refs\/tags\/v'\) && needs\.component-release\.result == 'success'\)\)/);
  assert.match(pages, /PUBLICATION_CHANNEL: .*'stable' \|\| 'nightly'/);
  assert.ok(pages.includes("queue: max"));
  assert.ok(pages.includes(`name: ${pagesDeployStep}`));
  assert.match(workflow, /CTL_BUNDLE_CHANNEL: .*'stable' \|\| 'nightly'/);
  assert.ok(workflow.includes("publish-component-release.mts desktop"));
  const release = workflow.split("  component-release:")[1]!.split("  bundle-pages:")[0]!;
  assert.ok(release.includes("needs: [metadata, remote-agent-set, remote-agent-windows]"));
  assert.ok(!release.includes("needs.remote-agent-windows.result"));
});

test("the only Pages publisher adds the landing page after staging or restoring bundles", async () => {
  const directory = new URL("../../.github/workflows/", import.meta.url);
  const workflows = (await readdir(directory)).filter((name) => /\.ya?ml$/.test(name));
  const publishers: string[] = [];
  for (const name of workflows) {
    const workflow = await readFile(new URL(name, directory), "utf8");
    if (/uses: actions\/deploy-pages@/.test(workflow)) publishers.push(name);
  }
  assert.deepEqual(publishers, ["bundles.yml"]);
  const workflow = await readFile(new URL("bundles.yml", directory), "utf8");
  const pages = workflow.split("  bundle-pages:")[1]!.split("  ctld:")[0]!;
  assert.match(pages, /inputs\.recover_pages && github\.ref == 'refs\/heads\/main'/);
  assert.match(pages, /PAGES_ARTIFACT_NAME: github-pages-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}/);
  assert.match(pages, /uses: actions\/upload-pages-artifact@v\d+\s+with:\s+name: \$\{\{ env\.PAGES_ARTIFACT_NAME \}\}/);
  assert.match(pages, /uses: actions\/deploy-pages@v\d+\s+with:\s+artifact_name: \$\{\{ env\.PAGES_ARTIFACT_NAME \}\}/);
  const stages = [
    "target/bundle-pages-site --snapshot-only",
    "Save complete site snapshot",
    "stage-pages-landing.mts target/bundle-pages-site apps/website/dist",
    "Upload combined Pages artifact",
    `name: ${pagesDeployStep}`,
    "for path in bundles/sets/*/*.tar.gz",
  ].map((step) => pages.indexOf(step));
  assert.ok(stages.every((position, index) => position >= 0 && (index === 0 || position > stages[index - 1]!)));
});
