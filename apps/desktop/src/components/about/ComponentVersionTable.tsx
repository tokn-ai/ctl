import { translate, type DisplayText, type Locale } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { componentBuildViews, componentHostFailure, componentHostStatuses, type ComponentHostGroup } from "../../features/about/componentVersions";
import type { ComponentVersionInfo, ComponentVersionRow, ComponentVersionStatus } from "../../lib/types";
import { ComponentProtocols } from "./ComponentProtocols";
import { InlineFeedback } from "../feedback/InlineFeedback";

const status_labels: Record<ComponentVersionStatus, string> = {
  current: "about.current",
  outdated: "about.outdated",
  newer: "about.newer",
  different_build: "about.different_build",
  incompatible: "about.protocol_mismatch",
  unknown: "about.unknown",
  not_running: "about.not_running",
  unavailable: "about.unavailable",
};

export function versionLabel(info: ComponentVersionInfo | null, locale: Locale = "en"): string {
  if (!info) return translate(locale, "about.unknown");
  const revision = info.source_revision?.slice(0, 10);
  return [info.version ?? translate(locale, "about.unknown_build_version"), revision, info.source_fingerprint ? translate(locale, "about.build_compact", { build: info.source_fingerprint.slice(0, 10) }) : null, info.dirty ? translate(locale, "about.modified") : null].filter(Boolean).join(" · ");
}

function versionDetails(info: ComponentVersionInfo | null, absent: string, locale: Locale): string {
  if (!info) return absent;
  return [
    translate(locale, "about.version_named", { version: info.version ?? translate(locale, "about.not_reported") }),
    translate(locale, "about.revision_detail", { revision: info.source_revision ?? translate(locale, "about.not_reported") }),
    translate(locale, "about.build_detail", { build: info.source_fingerprint ?? translate(locale, "about.not_reported") }),
    info.dirty === null ? null : info.dirty ? translate(locale, "about.modified_source") : translate(locale, "about.clean_source"),
  ].filter(Boolean).join("\n");
}

function Version({ info, absent }: { info: ComponentVersionInfo | null; absent: string }) {
  const { t, locale } = useLocale();
  return <span className="about-build-version" title={versionDetails(info, absent, locale)}>
    {info?.version ?? absent}
    {info?.source_revision ? <small className="about-build-id">{t("about.revision_named", { revision: info.source_revision.slice(0, 8) })}</small> : null}
    {info?.source_fingerprint ? <small className="about-build-id">{t("about.build_named", { build: info.source_fingerprint.slice(0, 10) })}</small> : null}
    {info?.dirty ? <small className="about-build-id">{t("about.modified_source")}</small> : null}
  </span>;
}

function statusLabel(row: ComponentVersionRow, locale: Locale): string {
  if (row.restart_required) return row.component === "ctl_agent" ? translate(locale, "about.reconnect_to_apply") : translate(locale, "about.restart_required");
  if (row.observation === "not_checked") return translate(locale, "about.not_checked");
  if (row.observation === "installed") return translate(locale, "about.on_demand");
  if (row.status !== "unknown" || !row.running) return translate(locale, status_labels[row.status]);
  return !row.running.source_revision && !row.running.source_fingerprint ? translate(locale, "about.build_not_reported") : translate(locale, "about.build_unverified");
}

export interface ComponentDiagnosticActions {
  busy_id: string | null;
  restarting: boolean;
  action_error: { component_id: string; message: DisplayText } | null;
  on_restart(component_id: string): void;
}

function ComponentRows({ row, busy_id, restarting, action_error, on_restart, hide_probe_error }: ComponentDiagnosticActions & { row: ComponentVersionRow; hide_probe_error: boolean }) {
  const { t, locale, formatText } = useLocale();
  const action = row.action;
  const action_label = action === "reconnect" ? t("about.reconnect") : t("about.restart");
  const details = [row.label, row.observation === "last_observed" ? t("about.last_observed") : null, row.detail].filter(Boolean).join("\n");
  const probe_error = hide_probe_error ? null : row.error;
  const errors = [probe_error ? row.location === "remote" ? t("about.check_component_failed") : probe_error : null, action_error?.component_id === row.component_id ? formatText(action_error.message) : null].filter(Boolean).join("\n");
  const views = componentBuildViews(row, locale);
  const label = row.location === "local" ? row.label : row.component === "ctl_agent" ? "ctl-agent" : row.component;
  return <tbody>{views.map((view, index) => <tr key={view.label} className={index === views.length - 1 ? "about-component-end" : "about-component-continuation"}>
    {index === 0 ? <th scope="rowgroup" rowSpan={views.length}>
      <div className="about-component-name">
        <strong title={details}>{label}</strong>
        {row.observation === "last_observed" ? <small className="about-muted">{t("about.active_connection")}</small> : null}
        {errors ? <div title={[probe_error, errors].filter(Boolean).join("\n")}><InlineFeedback className="about-row-feedback">{errors}</InlineFeedback></div> : null}
      </div>
    </th> : null}
    <td><span className="about-build-state">{view.label}</span><Version info={view.info} absent={t("about.unknown")} /></td>
    <td><ComponentProtocols row={row} view={view} /></td>
    {index === 0 ? <td rowSpan={views.length}><div className="about-row-actions">
      <span className={`about-version-status about-status-${row.status}`}>{statusLabel(row, locale)}</span>
      {action ? <button type="button" onClick={() => on_restart(row.component_id)} disabled={busy_id !== null} aria-label={t(action === "reconnect" ? "about.reconnect_component" : "about.restart_component", { name: row.label })}>
        {busy_id === row.component_id ? restarting ? action === "reconnect" ? t("about.reconnecting") : t("about.restarting") : t("about.checking") : action_label}
      </button> : null}
    </div></td> : null}
  </tr>)}</tbody>;
}

export function ComponentVersionTable({ group, ...actions }: ComponentDiagnosticActions & { group: ComponentHostGroup }) {
  const { t, locale } = useLocale();
  const failure = componentHostFailure(group.rows, locale);
  return <table className="about-version-table" aria-label={t("about.host_component_versions", { host: group.label })}>
    <colgroup><col className="about-component-column" /><col className="about-version-column" /><col className="about-protocol-column" /><col className="about-status-column" /></colgroup>
    <thead><tr><th scope="col">{t("about.component")}</th><th scope="col">{t("about.state_build")}</th><th scope="col">{t("about.protocols")}</th><th scope="col">{t("about.status_actions")}</th></tr></thead>
    {group.rows.map((row) => <ComponentRows key={row.component_id} row={row} {...actions} hide_probe_error={failure?.component_ids.has(row.component_id) ?? false} />)}
  </table>;
}

export function HostStatus({ group, action_error }: Pick<ComponentDiagnosticActions, "action_error"> & { group: ComponentHostGroup }) {
  const { t, locale } = useLocale();
  const statuses = componentHostStatuses(group.rows, locale);
  const failure = componentHostFailure(group.rows, locale);
  const maintenance_unsupported = group.rows.some((row) => row.error_code === "remote_windows_maintenance_unsupported");
  const has_action_error = action_error && group.rows.some((row) => row.component_id === action_error.component_id);
  return <>
    <div className="about-host-summary" role="group" aria-label={t("about.status")}>{statuses.map((status) => <span className={`about-version-status about-status-${status.status}`} key={status.label}>{status.label}</span>)}{has_action_error ? <span className="about-version-status about-status-incompatible">{t("about.action_failed")}</span> : null}</div>
    {failure ? <div title={failure.detail}><InlineFeedback severity={maintenance_unsupported ? "info" : "error"} className="about-row-feedback">{failure.message}</InlineFeedback></div> : null}
  </>;
}
