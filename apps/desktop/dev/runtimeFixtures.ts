import type {
  AboutRuntimeStatus, AboutRuntimeSyncResult, BundleChannel, ComponentBundle, ComponentBundlesSnapshot,
  ComponentUpdateCatalog, ComponentUpdateHostResult, ComponentUpdateProgress, ComponentUpdateRequest,
  ComponentUpdateSource,
} from "../src/lib/types";

const local_target = "aarch64-apple-darwin";
const remote_target = "x86_64-unknown-linux-musl";
const local_build = { id: "c".repeat(64), label: "dev-20261120T120000Z-v0.1.0-abcdef1-local", version: "0.1.0", revision: "abcdef1", channel: "local" };
const published = {
  stable: { id: "d".repeat(64), label: "stable-v0.1.1-fedcba0", version: "0.1.1", revision: "fedcba0", channel: "stable" },
  nightly: { id: "e".repeat(64), label: "nightly-20261122T120000Z-v0.1.1-abc1234", version: "0.1.1", revision: "abc1234", channel: "nightly" },
};
type Build = typeof local_build;

export function previewUpdateCatalog(channel: BundleChannel = "stable"): ComponentUpdateCatalog {
  const build = published[channel];
  return { default_channel: "stable", local_development: true, error: null, catalog: {
    channel, bundle_id: build.label, app_version: build.version, git_revision: build.revision,
    targets: [local_target, remote_target].map((target_triple) => ({ target_triple, compatible: true, components: {} })),
  } };
}

/** All changes to the preview's runtime inventory live only for this page load. */
export function previewRuntime(bundles: ComponentBundlesSnapshot, partial: boolean) {
  const active = new Map<string | null, Build>([
    [null, local_build],
    ["preview-dev", { id: "f".repeat(64), label: "stable-v0.1.0-123456a", version: "0.1.0", revision: "123456a", channel: "stable" }],
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
    if (host_id !== null) return;
    for (const bundle of bundles.bundles) {
      if (bundle.local_use === "selected") bundle.local_use = "available";
    }
    const installed = bundles.bundles.find((bundle) => bundle.bundle_id === build.id && bundle.target_triple === local_target);
    if (installed) installed.local_use = "selected";
    else bundles.bundles.push({
      bundle_id: build.id, target_triple: local_target, source: build.channel === "local" ? "local" : "release",
      app_version: build.version, git_revision: build.revision, dirty: build.channel === "local", compatible: true,
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
      return { active: build?.id ?? null, build_label: build?.label ?? null, origin: build ? { channel: build.channel } : null, missing: false };
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
