import type {
  AboutRuntimeStatus, AboutRuntimeSyncResult, BundleChannel, ComponentBundle, ComponentBundlesSnapshot,
  ComponentUpdateCatalog, ComponentUpdateHostResult, ComponentUpdateProgress, ComponentUpdateRequest,
  ComponentUpdateSource, ComponentVersionsSnapshot,
} from "../src/lib/types";

const local_target = "aarch64-apple-darwin";
const remote_target = "x86_64-unknown-linux-musl";
const published_targets = [local_target, remote_target, "aarch64-unknown-linux-musl", "x86_64-pc-windows-msvc"];

function identity(version: string, revision: string, fingerprint: string) {
  return { version, source_revision: revision.repeat(5), source_fingerprint: fingerprint.repeat(8), dirty: false };
}

// Both the inventory and service probes report these complete fictional identities.
export const sample_builds = {
  current: identity("0.1.0", "abcdef12", "9d314f08"),
  older_stable: identity("0.0.9", "123456ab", "37ce621a"),
  older_nightly: identity("0.1.0", "abcdef12", "c5891e4a"),
};
const local_build = { id: "c".repeat(64), label: "dev-20261120T120000Z-v0.1.0-abcdef1-local", identity: sample_builds.current, channel: "local" };
const remote_build = { id: "f".repeat(64), label: "stable-v0.1.0-abcdef1", identity: sample_builds.current, channel: "stable" };
const published = {
  stable: { id: "d".repeat(64), label: "stable-v0.1.1-fedcba0", identity: identity("0.1.1", "fedcba01", "dedcba01"), channel: "stable" },
  nightly: { id: "e".repeat(64), label: "nightly-20261122T120000Z-v0.1.1-abc1234", identity: identity("0.1.1", "abc12345", "abcd1234"), channel: "nightly" },
};
type Build = typeof local_build;
type RetainedBuild = { build: Build["identity"]; channels: string[] };

function retain(entries: RetainedBuild[], build: Build) {
  const record = entries.find((entry) => entry.build === build.identity);
  if (!record) entries.push({ build: build.identity, channels: [build.channel] });
  else if (!record.channels.includes(build.channel)) record.channels.push(build.channel);
}

export function previewUpdateCatalog(channel: BundleChannel = "stable"): ComponentUpdateCatalog {
  const build = published[channel];
  return { default_channel: "stable", local_development: true, error: null, catalog: {
    channel, publication_id: build.label, bundle_id: build.label, app_version: build.identity.version, git_revision: build.identity.source_revision,
    targets: published_targets.map((target_triple) => ({ target_triple, compatible: true, components: {} })),
  } };
}

/** All changes to the preview's runtime inventory live only for this page load. */
export function previewRuntime(bundles: ComponentBundlesSnapshot, observations: ComponentVersionsSnapshot, partial: boolean) {
  const active = new Map<string | null, Build>([
    [null, local_build],
    ["preview-dev", remote_build],
  ]);
  const retained = new Map<string | null, RetainedBuild[]>([
    [null, [
      { build: sample_builds.current, channels: ["local"] },
      { build: sample_builds.older_stable, channels: ["stable"] },
      { build: sample_builds.older_nightly, channels: ["nightly"] },
    ]],
    ["preview-dev", [
      { build: sample_builds.current, channels: ["stable"] },
      { build: sample_builds.older_nightly, channels: ["nightly"] },
    ]],
  ]);
  const local_store = new Map<string, RetainedBuild[]>([
    [local_target, retained.get(null)!],
    [remote_target, [{ build: sample_builds.current, channels: ["stable"] }]],
  ]);
  const local_publications = new Map<string, Record<string, string>>([
    [remote_target, { stable: remote_build.label }],
  ]);
  const known = new Map<string, Build>([local_build, ...Object.values(published)].map((build) => [build.id, build]));
  const pending = new Map<string, () => void>();

  function wait(attempt_id: string): Promise<boolean> {
    return new Promise((resolve) => {
      const timer = setTimeout(() => { pending.delete(attempt_id); resolve(true); }, 750);
      pending.set(attempt_id, () => { clearTimeout(timer); pending.delete(attempt_id); resolve(false); });
    });
  }

  function read(host_id: string | null): Build | undefined {
    if (partial && host_id === "preview-staging") throw new Error("The sample SSH connection timed out.");
    return active.get(host_id);
  }

  function activate(host_id: string | null, build: Build) {
    active.set(host_id, build);
    known.set(build.id, build);
    const inventory = retained.get(host_id) ?? [];
    retain(inventory, build);
    retained.set(host_id, inventory);
    for (const row of observations.components) {
      if (row.host_id !== host_id || row.component === "ctmux" || !row.installed) continue;
      row.installed = { ...row.installed, ...build.identity };
      if (row.location === "local") row.available = row.installed;
      if (row.running?.source_fingerprint) row.restart_required = row.running.source_fingerprint !== build.identity.source_fingerprint;
    }
    if (host_id !== null) return;
    if (build.channel === "stable" || build.channel === "nightly") {
      local_publications.set(local_target, { ...local_publications.get(local_target), [build.channel]: build.label });
    }
    for (const bundle of bundles.bundles) {
      if (bundle.local_use === "selected") bundle.local_use = "available";
    }
    const installed = bundles.bundles.find((bundle) => bundle.bundle_id === build.id && bundle.target_triple === local_target);
    if (installed) installed.local_use = "selected";
    else bundles.bundles.push({
      bundle_id: build.id, target_triple: local_target, source: build.channel === "local" ? "local" : "release",
      app_version: build.identity.version, git_revision: build.identity.source_revision, dirty: build.identity.dirty, compatible: true,
      included: false, local_use: "selected", local_unavailable_reason: null, upload_use: "available", upload_unavailable_reason: null,
    });
  }

  function chosenBuild(source: ComponentUpdateSource, host_id: string | null): Build {
    if (source.kind === "published") return published[source.channel];
    if (source.kind === "working_tree" || source.kind === "provided" && source.local_build) return local_build;
    if (source.kind === "provided") return { ...local_build, id: "1".repeat(64), channel: "provided", label: "provided-undated-v0.1.0-abcdef1" };
    if (source.kind === "selected") {
      const included = bundles.bundles.find((bundle) => bundle.included && bundle.target_triple === (host_id === null ? local_target : remote_target));
      if (!included) throw new Error("No sample build is included for this platform.");
      if (host_id === null && included.local_use === "unavailable") throw new Error(included.local_unavailable_reason ?? "This build is unavailable.");
      return { ...local_build, id: included.bundle_id, channel: "provided", label: "provided-undated-v0.1.0-abcdef1" };
    }
    return host_id === null ? active.get(null) ?? published.stable : published.stable;
  }

  return {
    status(host_id: string | null): AboutRuntimeStatus {
      const build = read(host_id);
      return {
        active: build?.id ?? null, build_label: build?.label ?? null, origin: build ? { channel: build.channel } : null,
        missing: false, build_channels: structuredClone(retained.get(host_id) ?? []),
        target_triple: host_id === null ? local_target : build ? remote_target : null, active_build: build?.identity ?? null,
        installed_targets: host_id === null ? Array.from(local_store, ([target_triple, builds]) => ({
          target_triple, builds: structuredClone(builds), current_publications: { ...local_publications.get(target_triple) },
        })) : undefined,
      };
    },
    async fetch(target_triple: string, channel: BundleChannel, attempt_id: string) {
      if (!published_targets.includes(target_triple)) throw new Error("This target is absent from the sample publication.");
      if (!await wait(attempt_id)) throw { code: "runtime_operation_cancelled", message: "Target download stopped." };
      if (!local_store.has(target_triple)) local_store.set(target_triple, []);
      for (const [target, entries] of local_store) {
        if (target === target_triple || entries.some((entry) => entry.channels.includes(channel))) {
          retain(entries, published[channel]);
          local_publications.set(target, { ...local_publications.get(target), [channel]: published[channel].label });
        }
      }
    },
    selectLocal(bundle: ComponentBundle) {
      const build = known.get(bundle.bundle_id);
      if (build) activate(null, build);
    },
    cancel(attempt_id: string) { pending.get(attempt_id)?.(); },
    async sync(host_id: string | null, attempt_id: string): Promise<AboutRuntimeSyncResult> {
      if (!await wait(attempt_id)) throw new Error("Runtime sync was stopped.");
      const build = read(host_id);
      if (!build) throw new Error("Install a runtime to choose its source first.");
      if (build.channel !== "stable" && build.channel !== "nightly") return { state: "pinned" };
      const latest = published[build.channel];
      if (build.id === latest.id) return { state: "current" };
      activate(host_id, latest);
      return { state: "installed" };
    },
    async install(request: ComponentUpdateRequest, progress: (event: ComponentUpdateProgress) => void) {
      const results: ComponentUpdateHostResult[] = [];
      let cancelled = false;
      for (const [host_index, target] of request.targets.entries()) {
        if (!cancelled) {
          progress({ host_index, state: "updating", progress: null });
          cancelled = !await wait(request.attempt_id);
        }
        if (cancelled) {
          results.push({ host_index, state: "cancelled", result: null, error: null });
          continue;
        }
        try {
          const host_id = target.kind === "ssh" ? target.host_id ?? target.destination : null;
          read(host_id);
          const build = chosenBuild(request.options.source, host_id);
          activate(host_id, build);
          results.push({ host_index, state: "complete", result: {
            package: request.options.package, bundle_id: build.id, target_triple: host_id === null ? local_target : remote_target, services_preserved: true,
          }, error: null });
        } catch (error) {
          results.push({ host_index, state: "failed", error: error instanceof Error ? error.message : String(error), result: null });
        }
      }
      return results;
    },
  };
}
