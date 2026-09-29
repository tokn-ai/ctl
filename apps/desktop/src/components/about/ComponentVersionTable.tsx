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
  rmux: "Session", rmux_control: "Local control", task: "Task", task_control: "Task control",
  ctld: "ctld IPC", ctld_lifecycle: "Lifecycle", ctl_identity: "Agent identity",
};

export function versionLabel(info: ComponentVersionInfo | null): string {
  if (!info) return "Unknown";
  const revision = info.source_revision?.slice(0, 10);
  return [info.version ?? "Unknown version", revision, info.source_fingerprint ? `build ${info.source_fingerprint.slice(0, 10)}` : null, info.dirty ? "modified" : null].filter(Boolean).join(" · ");
}

function Version({ info, absent }: { info: ComponentVersionInfo | null; absent: string }) {
  if (!info) return <span className="about-muted">{absent}</span>;
  return <>
    <span>{info.version ?? "Unknown version"}</span>
    {info.source_revision || info.dirty ? <small title={info.source_revision ?? undefined}>{[info.source_revision?.slice(0, 10), info.dirty ? "Modified source" : null].filter(Boolean).join(" · ")}</small> : null}
    {info.source_fingerprint ? <small title={info.source_fingerprint}>Build {info.source_fingerprint.slice(0, 10)}</small> : null}
  </>;
}

function Protocols({ row }: { row: ComponentVersionRow }) {
  const actual = row.running?.protocols ?? [];
  const required = row.required_protocols ?? row.available?.protocols ?? [];
  if (!actual.length) return <><span className="about-muted">Unknown</span>{required.map((protocol) => <small key={protocol.name}>Requires {protocol_labels[protocol.name] ?? protocol.name} {protocol.version}</small>)}</>;
  return <>
    {actual.map((protocol) => {
      const expected = required.find((candidate) => candidate.name === protocol.name)?.version;
      return <div key={protocol.name} className="about-protocol"><span>{protocol_labels[protocol.name] ?? protocol.name} {protocol.version}</span>{expected !== undefined && expected !== protocol.version ? <small>Requires {expected}</small> : null}</div>;
    })}
    {required.filter((protocol) => !actual.some((candidate) => candidate.name === protocol.name)).map((protocol) => <div key={protocol.name} className="about-protocol"><span className="about-muted">{protocol_labels[protocol.name] ?? protocol.name} unknown</span><small>Requires {protocol.version}</small></div>)}
  </>;
}

interface Props {
  rows: readonly ComponentVersionRow[];
  busy_id: string | null;
  restarting: boolean;
  action_error: { component_id: string; message: string } | null;
  on_restart(component_id: string): void;
  reference_label?: string;
}

export function ComponentVersionTable({ rows, busy_id, restarting, action_error, on_restart, reference_label = "Available" }: Props) {
  return <div className="about-table-scroll"><table className="about-version-table">
    <thead><tr><th>Component</th><th>Version</th><th>Protocol</th><th>{reference_label}</th><th>Status</th></tr></thead>
    <tbody>{rows.map((row) => <tr key={row.component_id}>
      <th scope="row">
        <strong>{row.label}</strong>
        {row.observation === "last_observed" ? <small>Last observed</small> : null}
        {row.detail ? <small>{row.detail}</small> : null}
        {row.error ? <p className="about-row-error" role="alert">{row.error}</p> : null}
        {action_error?.component_id === row.component_id ? <p className="about-row-error" role="alert">{action_error.message}</p> : null}
      </th>
      <td><Version info={row.running} absent={row.status === "not_running" ? "Not running" : "Unknown"} /></td>
      <td><Protocols row={row} /></td>
      <td><Version info={row.available} absent="Unknown" /></td>
      <td><span className={`about-version-status about-status-${row.status}`}>{status_labels[row.status]}</span>
        {row.component === "ctld" && row.restart_supported ? <button type="button" onClick={() => on_restart(row.component_id)} disabled={busy_id !== null} aria-label={`Restart ${row.label}`}>
          {busy_id === row.component_id ? restarting ? "Restarting…" : "Checking…" : "Restart ctld"}
        </button> : null}
      </td>
    </tr>)}</tbody>
  </table></div>;
}
