import type { ComponentBundle, ComponentBundlesSnapshot, ComponentProtocolVersion, ComponentVersionInfo, ComponentVersionRow, ComponentVersionsSnapshot } from "../src/lib/types";

const protocol = (name: string, version: string, supported_versions = [version]): ComponentProtocolVersion => ({ name, version, build: Number(version.split(".")[2]), supported_versions });
const current: ComponentVersionInfo = { version: "0.1.0", source_revision: "abcdef1234567890", source_fingerprint: "current-build-preview", dirty: false, protocols: [] };
const info = (name: string, version: string): ComponentVersionInfo => ({ ...current, protocols: [protocol(name, version)] });
const row = (component_id: string, component: ComponentVersionRow["component"], label: string, running: ComponentVersionInfo, status: ComponentVersionRow["status"] = "current"): ComponentVersionRow => ({
  component_id, component, label, location: "local", host_id: null, observation: "running", status,
  running, available: running, installed: running, required_protocols: running.protocols,
  restart_supported: component !== "ctmux" && component !== "ctl_agent", action: component === "ctmux" ? null : component === "ctl_agent" ? "reconnect" : "restart", detail: null, error: null,
});

/** These observations are fictional; preview never queries or restarts a daemon. */
export function previewComponentVersions(): ComponentVersionsSnapshot {
  const agent = { ...current, protocols: [protocol("ctl_identity", "1.0.3"), protocol("ctl_maintenance", "1.1.3", ["1.0.2", "1.1.3"])] };
  return { components: [
    { ...row("app", "ctmux", "ctmux", { ...current, source_fingerprint: null, dirty: null }), observation: "bundled" },
    { ...row("ctld-local", "ctld", "ctld (SSH, VPN)", { ...info("ctld", "1.1.13"), version: "0.0.9", source_revision: "123456abcdef", source_fingerprint: "older-build-preview" }, "outdated"), available: info("ctld", "1.1.13"), installed: info("ctld", "1.1.13"), restart_required: true, detail: "Connection broker for SSH, port forwards, and VPNs." },
    { ...row("ctmuxd-local", "ctmuxd", "ctmuxd", { ...info("ctmux", "1.0.13"), source_fingerprint: "changed-build-preview", dirty: true }, "different_build"), available: info("ctmux", "1.0.13"), installed: info("ctmux", "1.0.13"), restart_required: true, detail: "Terminal sessions" },
    { ...row("ctl-taskd-local", "ctl-taskd", "ctl-taskd", info("task", "1.0.3")), detail: "Task execution" },
    { ...row("remote-agent", "ctl_agent", "ctl-agent — Development", { ...agent, source_revision: "112233445566", source_fingerprint: "remote-build-preview" }, "different_build"), location: "remote", host_id: "dev-server", host_key: "saved:dev-server", host_name: "Development", connected: true, observation: "last_observed", available: agent, installed: agent, detail: "Connected SSH environment" },
    { ...row("remote-ctmux", "ctmuxd", "ctmuxd — Development", info("ctmux", "1.0.13")), location: "remote", host_id: "dev-server", host_key: "saved:dev-server", host_name: "Development", connected: true, observation: "last_observed" },
  ] };
}

export function previewComponentBundles(): ComponentBundlesSnapshot {
  const included: ComponentBundle = {
    bundle_id: "a".repeat(64), target_triple: "aarch64-apple-darwin", source: "ci", app_version: "0.1.0",
    git_revision: "abcdef1234567890", dirty: false, compatible: true, included: true,
    local_use: "unavailable", local_unavailable_reason: "Requires a signed macOS helper package",
    upload_use: "available", upload_unavailable_reason: null,
  };
  return { bundles: [
    included,
    { ...included, bundle_id: "b".repeat(64), target_triple: "x86_64-unknown-linux-musl", local_unavailable_reason: "For another platform" },
    { ...included, bundle_id: "c".repeat(64), source: "local", dirty: true, included: false, local_use: "selected", local_unavailable_reason: null },
  ], errors: [] };
}
