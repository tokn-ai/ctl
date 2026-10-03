import type { ComponentVersionInfo, ComponentVersionRow, ComponentVersionStatus } from "../../lib/types";

const status_labels: Record<ComponentVersionStatus, string> = {
  current: "Current",
  outdated: "Outdated",
  newer: "Newer",
  different_build: "Different build",
  incompatible: "Protocol mismatch",
  unknown: "Unknown",
  not_running: "Not running",
  unavailable: "Unavailable",
};

const protocol_labels: Record<string, string> = {
  ctmux: "Session", ctmux_control: "Local control", task: "Task", task_control: "Task control",
  ctld: "ctld IPC", ctld_lifecycle: "Lifecycle", ctl_identity: "Agent identity", ctl_maintenance: "Remote maintenance", ctld_helper: "Helper API",
};

export function versionLabel(info: ComponentVersionInfo | null): string {
  if (!info) return "Unknown";
  const revision = info.source_revision?.slice(0, 10);
  return [info.version ?? "Unknown version", revision, info.source_fingerprint ? `build ${info.source_fingerprint.slice(0, 10)}` : null, info.dirty ? "modified" : null].filter(Boolean).join(" · ");
}

function versionDetails(info: ComponentVersionInfo | null, absent: string): string {
  if (!info) return absent;
  return [
    `Version: ${info.version ?? "Not reported"}`,
    `Revision: ${info.source_revision ?? "Not reported"}`,
    `Build: ${info.source_fingerprint ?? "Not reported"}`,
    info.dirty === null ? null : info.dirty ? "Modified source" : "Clean source",
  ].filter(Boolean).join("\n");
}

function Version({ info, absent }: { info: ComponentVersionInfo | null; absent: string }) {
  const build = info?.source_revision?.slice(0, 8) ?? info?.source_fingerprint?.slice(0, 8);
  return <span title={versionDetails(info, absent)}>{info?.version ?? absent}{build ? <small className="about-build-id">{build}{info?.dirty ? " · modified" : ""}</small> : null}</span>;
}

function statusLabel(row: ComponentVersionRow): string {
  if (row.restart_required) return row.component === "ctl_agent" ? "Reconnect to apply" : "Restart required";
  if (row.observation === "not_checked") return "Not checked";
  if (row.observation === "installed") return "On demand";
  if (row.status !== "unknown" || !row.running) return status_labels[row.status];
  return !row.running.source_revision && !row.running.source_fingerprint ? "Build not reported" : "Build unverified";
}

const compact_protocol_labels: Record<string, string> = {
  ctmux: "Session", ctmux_control: "Control", task: "Task", task_control: "Control",
  ctld: "IPC", ctld_lifecycle: "Lifecycle", ctl_identity: "Identity", ctl_maintenance: "Maintenance", ctld_helper: "Helper",
};

function Protocols({ row }: { row: ComponentVersionRow }) {
  const actual = (row.observation === "installed" ? row.installed?.protocols : row.running?.protocols) ?? [];
  const required = row.required_protocols ?? row.available?.protocols ?? [];
  const observed = actual.map((protocol) => {
    const expected = required.find((candidate) => candidate.name === protocol.name);
    const requirement = expected ? ` (requires ${expected.supported_versions.join(" or ")})` : "";
    return `${protocol_labels[protocol.name] ?? protocol.name} ${protocol.version}${requirement}; build ${protocol.build}; supports ${protocol.supported_versions.join(", ")}`;
  });
  const missing = required.filter((protocol) => !actual.some((candidate) => candidate.name === protocol.name));
  const detail = [...observed, ...missing.map((protocol) => `${protocol_labels[protocol.name] ?? protocol.name}: not reported (requires ${protocol.supported_versions.join(" or ")})`)];
  const legacy = row.legacy_protocols ?? [];
  const summary = [
    ...legacy.map((protocol) => `${compact_protocol_labels[protocol.name] ?? protocol.name} legacy ${protocol.version}`),
    ...actual.map((protocol) => `${compact_protocol_labels[protocol.name] ?? protocol.name} ${protocol.version}`),
    ...missing.map((protocol) => `${compact_protocol_labels[protocol.name] ?? protocol.name} ?`),
  ].join(" · ");
  return <span className="about-protocols" title={[...legacy.map((p) => `${p.name}: historical numeric protocol ${p.version}, not a published contract`), ...detail].join("\n") || "Protocols not reported"}>{summary || "Unknown"}</span>;
}

interface Props {
  rows: readonly ComponentVersionRow[];
  busy_id: string | null;
  restarting: boolean;
  action_error: { component_id: string; message: string } | null;
  on_restart(component_id: string): void;
  manageable_host_ids?: readonly string[];
  on_manage_host?(host_id: string, mode: "inspect" | "update"): void;
}

export function ComponentVersionTable({ rows, busy_id, restarting, action_error, on_restart, manageable_host_ids = [], on_manage_host }: Props) {
  return <div className="about-table-scroll"><table className="about-version-table">
    <colgroup><col className="about-component-column" /><col className="about-version-column" /><col className="about-protocol-column" /><col className="about-version-column" /><col className="about-status-column" /></colgroup>
    <thead><tr><th>Component</th><th>Running</th><th>Protocol</th><th>Installed</th><th>Status / actions</th></tr></thead>
    <tbody>{rows.map((row) => {
      const action = row.action;
      const action_label = action === "reconnect" ? "Reconnect" : "Restart";
      const details = [row.label, row.observation === "last_observed" ? "Last observed" : null, row.detail].filter(Boolean).join("\n");
      const errors = [row.error, action_error?.component_id === row.component_id ? action_error.message : null].filter(Boolean).join("\n");
      return <tr key={row.component_id}>
        <th scope="row">
          <div className="about-component-name">
            <strong title={details}>{row.label}</strong>
            {errors ? <span className="about-row-error" role="alert" title={errors}>{errors}</span> : null}
          </div>
        </th>
        <td><Version info={row.running} absent={row.observation === "installed" ? "On demand" : row.status === "not_running" ? "Not running" : "Unknown"} /></td>
        <td><Protocols row={row} /></td>
        <td><Version info={row.installed === undefined && row.location === "local" ? row.available : row.installed ?? null} absent="Unknown" /></td>
        <td><div className="about-row-actions">
          <span className={`about-version-status about-status-${row.status}`}>{statusLabel(row)}</span>
          {action ? <button type="button" onClick={() => on_restart(row.component_id)} disabled={busy_id !== null} aria-label={`${action_label} ${row.label}`}>
            {busy_id === row.component_id ? restarting ? action === "reconnect" ? "Reconnecting…" : "Restarting…" : "Checking…" : action_label}
          </button> : null}
          {row.component === "ctl_agent" && row.host_id && manageable_host_ids.includes(row.host_id) && row.component_id.startsWith("remote:saved:") ? <>
            <button type="button" disabled={busy_id !== null} onClick={() => on_manage_host?.(row.host_id!, "inspect")} aria-label={`Check host ${row.label}`}>Check host</button>
            <button type="button" disabled={busy_id !== null} onClick={() => on_manage_host?.(row.host_id!, "update")} aria-label={`Update components ${row.label}`}>Update…</button>
          </> : null}
        </div></td>
      </tr>;
    })}</tbody>
  </table></div>;
}
