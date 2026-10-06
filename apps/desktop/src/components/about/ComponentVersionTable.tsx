import { useId, useState } from "react";
import { componentBuildViews, componentHostFailure, componentHostStatuses, groupComponentHosts, type ComponentHostGroup } from "../../features/about/componentVersions";
import type { ComponentVersionInfo, ComponentVersionRow, ComponentVersionStatus } from "../../lib/types";
import { Icon } from "../ui/Icon";
import { ComponentProtocols } from "./ComponentProtocols";

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
  return <span className="about-build-version" title={versionDetails(info, absent)}>
    {info?.version ?? absent}
    {info?.source_revision ? <small className="about-build-id">Revision {info.source_revision.slice(0, 8)}</small> : null}
    {info?.source_fingerprint ? <small className="about-build-id">Build {info.source_fingerprint.slice(0, 10)}</small> : null}
    {info?.dirty ? <small className="about-build-id">Modified source</small> : null}
  </span>;
}

function statusLabel(row: ComponentVersionRow): string {
  if (row.restart_required) return row.component === "ctl_agent" ? "Reconnect to apply" : "Restart required";
  if (row.observation === "not_checked") return "Not checked";
  if (row.observation === "installed") return "On demand";
  if (row.status !== "unknown" || !row.running) return status_labels[row.status];
  return !row.running.source_revision && !row.running.source_fingerprint ? "Build not reported" : "Build unverified";
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

type ActionProps = Pick<Props, "busy_id" | "restarting" | "action_error" | "on_restart">;

function ComponentRows({ row, busy_id, restarting, action_error, on_restart, hide_probe_error }: ActionProps & { row: ComponentVersionRow; hide_probe_error: boolean }) {
  const action = row.action;
  const action_label = action === "reconnect" ? "Reconnect" : "Restart";
  const details = [row.label, row.observation === "last_observed" ? "Last observed" : null, row.detail].filter(Boolean).join("\n");
  const probe_error = hide_probe_error ? null : row.error;
  const errors = [probe_error ? row.location === "remote" ? "Could not check this component." : probe_error : null, action_error?.component_id === row.component_id ? action_error.message : null].filter(Boolean).join("\n");
  const views = componentBuildViews(row);
  const label = row.location === "local" ? row.label : row.component === "ctl_agent" ? "ctl-agent" : row.component;
  return <tbody>{views.map((view, index) => <tr key={view.label} className={index === views.length - 1 ? "about-component-end" : "about-component-continuation"}>
    {index === 0 ? <th scope="rowgroup" rowSpan={views.length}>
      <div className="about-component-name">
        <strong title={details}>{label}</strong>
        {row.observation === "last_observed" ? <small className="about-muted">Active connection</small> : null}
        {errors ? <span className="about-row-error" role="alert" title={[probe_error, errors].filter(Boolean).join("\n")}>{errors}</span> : null}
      </div>
    </th> : null}
    <td><span className="about-build-state">{view.label}</span><Version info={view.info} absent="Unknown" /></td>
    <td><ComponentProtocols row={row} view={view} /></td>
    {index === 0 ? <td rowSpan={views.length}><div className="about-row-actions">
      <span className={`about-version-status about-status-${row.status}`}>{statusLabel(row)}</span>
      {action ? <button type="button" onClick={() => on_restart(row.component_id)} disabled={busy_id !== null} aria-label={`${action_label} ${row.label}`}>
        {busy_id === row.component_id ? restarting ? action === "reconnect" ? "Reconnecting…" : "Restarting…" : "Checking…" : action_label}
      </button> : null}
    </div></td> : null}
  </tr>)}</tbody>;
}

function ComponentDetails({ group, ...actions }: ActionProps & { group: ComponentHostGroup }) {
  const failure = componentHostFailure(group.rows);
  return <table className="about-version-table" aria-label={`${group.label} component versions`}>
    <colgroup><col className="about-component-column" /><col className="about-version-column" /><col className="about-protocol-column" /><col className="about-status-column" /></colgroup>
    <thead><tr><th scope="col">Component</th><th scope="col">State / build</th><th scope="col">Protocols</th><th scope="col">Status / actions</th></tr></thead>
    {group.rows.map((row) => <ComponentRows key={row.component_id} row={row} {...actions} hide_probe_error={failure?.component_ids.has(row.component_id) ?? false} />)}
  </table>;
}

function HostStatus({ group, action_error }: Pick<Props, "action_error"> & { group: ComponentHostGroup }) {
  const statuses = componentHostStatuses(group.rows);
  const failure = componentHostFailure(group.rows);
  const has_action_error = action_error && group.rows.some((row) => row.component_id === action_error.component_id);
  return <>
    <div className="about-host-summary">{statuses.map((status) => <span className={`about-version-status about-status-${status.status}`} key={status.label}>{status.label}</span>)}{has_action_error ? <span className="about-version-status about-status-incompatible">Action failed</span> : null}</div>
    {failure ? <p className="about-host-error" role="alert" title={failure.detail}>{failure.message}</p> : null}
  </>;
}

export function ComponentVersionTable({ rows, busy_id, restarting, action_error, on_restart, manageable_host_ids = [], on_manage_host }: Props) {
  const [expanded, setExpanded] = useState<string | null>(null);
  const id = useId();
  const groups = groupComponentHosts(rows);
  const local = groups.filter((group) => group.key === "local");
  const remote = groups.filter((group) => group.key !== "local");
  const actions = { busy_id, restarting, action_error, on_restart };
  return <>
    {local.map((group) => <section key={group.key} aria-label={`${group.label} components`}>
      <div className="about-table-scroll"><ComponentDetails group={group} {...actions} /></div>
    </section>)}
    {remote.length ? <div className="about-table-scroll"><table className="about-host-table" aria-label="Remote component hosts">
      <colgroup><col className="about-host-name-column" /><col className="about-host-status-column" /><col className="about-host-action-column" /></colgroup>
      <thead><tr><th scope="col">Host</th><th scope="col">Status</th><th scope="col">Actions</th></tr></thead>
      {remote.map((group) => {
        const open = expanded === group.key;
        const table_id = `${id}-${encodeURIComponent(group.key)}`;
        const manageable = group.host_id && manageable_host_ids.includes(group.host_id) && on_manage_host;
        return <tbody key={group.key}>
          <tr className="about-host-row">
            <th scope="row"><button type="button" className="about-host-toggle" onClick={() => setExpanded((previous) => previous === group.key ? null : group.key)} aria-expanded={open} aria-controls={table_id} aria-label={`${open ? "Hide" : "Show"} components for ${group.label}`}>
              <Icon name={open ? "chevron_down" : "chevron_right"} />
              <strong>{group.label}</strong>
            </button></th>
            <td><HostStatus group={group} action_error={action_error} /></td>
            <td>{manageable ? <div className="about-host-actions">
              <button type="button" disabled={busy_id !== null} onClick={() => on_manage_host(group.host_id!, "inspect")} aria-label={`Check host ${group.label}`}>Check host</button>
              <button type="button" disabled={busy_id !== null} onClick={() => on_manage_host(group.host_id!, "update")} aria-label={`Update components ${group.label}`}>Update…</button>
            </div> : <span className="about-muted">—</span>}</td>
          </tr>
          <tr hidden={!open} className="about-host-details"><td colSpan={3}><div id={table_id}><ComponentDetails group={group} {...actions} /></div></td></tr>
        </tbody>;
      })}
    </table></div> : null}
  </>;
}
