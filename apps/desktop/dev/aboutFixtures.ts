import type { ComponentVersionInfo, ComponentVersionRow, ComponentVersionsSnapshot } from "../src/lib/types";

const current: ComponentVersionInfo = { version: "0.1.0", source_revision: "abcdef1234567890", source_fingerprint: "current-build-preview", dirty: false, protocols: [] };
const info = (protocol: string, version: number): ComponentVersionInfo => ({ ...current, protocols: [{ name: protocol, version }] });
const row = (component_id: string, component: ComponentVersionRow["component"], label: string, running: ComponentVersionInfo, status: ComponentVersionRow["status"] = "current"): ComponentVersionRow => ({
  component_id, component, label, location: "local", host_id: null, observation: "running", status,
  running, available: running, restart_supported: component !== "rmux" && component !== "ctl_agent", action: component === "rmux" ? null : component === "ctl_agent" ? "reconnect" : "restart", detail: null, error: null,
});

/** These observations are fictional; preview never queries or restarts a daemon. */
export function previewComponentVersions(): ComponentVersionsSnapshot {
  return { components: [
    { ...row("app", "rmux", "rmux", { ...current, source_fingerprint: null, dirty: null }), observation: "bundled" },
    { ...row("ctld-local", "ctld", "ctld", { ...info("ctld", 11), version: "0.0.9", source_revision: "123456abcdef", source_fingerprint: "older-build-preview" }, "outdated"), available: info("ctld", 11), detail: "SSH and VPN broker" },
    { ...row("rmuxd-local", "rmuxd", "rmuxd", { ...info("rmux", 12), source_fingerprint: "changed-build-preview", dirty: true }, "different_build"), available: info("rmux", 12), detail: "Terminal sessions" },
    { ...row("taskd-local", "taskd", "taskd", info("task", 3)), detail: "Task execution" },
    { ...row("remote-agent", "ctl_agent", "Development · ctl-agent", { ...current, source_revision: "112233445566", source_fingerprint: "remote-build-preview", protocols: [{ name: "ctl_identity", version: 2 }] }, "different_build"), location: "remote", host_id: "dev-server", observation: "last_observed", available: { ...current, protocols: [{ name: "ctl_identity", version: 2 }] }, detail: "Connected SSH environment" },
    { ...row("remote-rmux", "rmuxd", "Development · rmuxd", info("rmux", 12)), location: "remote", host_id: "dev-server", observation: "last_observed" },
  ] };
}
