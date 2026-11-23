import { useId, useState } from "react";
import { translate, type DisplayText, type Locale } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { componentBuildChannels, componentBuildViews, componentHostFailure, componentHostStatuses, componentProtocolLines, type ComponentBuildView, type ComponentHostGroup } from "../../features/about/componentVersions";
import type { AboutRuntimeStatus, ComponentVersionInfo, ComponentVersionRow, ComponentVersionStatus } from "../../lib/types";
import { ComponentProtocols } from "./ComponentProtocols";
import { InlineFeedback } from "../feedback/InlineFeedback";
import { Icon } from "../ui/Icon";

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

function statusLabel(row: ComponentVersionRow, locale: Locale): string {
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

interface ChannelInventory {
  build_channels: AboutRuntimeStatus["build_channels"];
  channel_unchecked: boolean;
}

// Keep Git revisions usable as commit prefixes. Long fingerprint comparisons
// can show the distinguishing digits directly because they aren't Git refs.
function differingBuildIds([owner, disk]: ComponentBuildView[]) {
  if (!owner.info || !disk?.info || owner.info.version !== disk.info.version) return;
  for (const field of ["source_revision", "source_fingerprint"] as const) {
    const left = owner.info[field];
    const right = disk.info[field];
    if (!left || !right || left === right) continue;
    let difference = 0;
    while (left[difference] === right[difference]) difference++;
    const length = Math.max(8, difference + 1);
    const values = [left, right].map((id) => field === "source_revision" || length <= 12
      ? id.slice(0, length) : `${id.slice(0, 7)}…${id.slice(difference - 1, difference + 2)}`);
    return { field, values };
  }
}

function ComponentRows({ row, busy_id, restarting, action_error, on_restart, hide_probe_error, build_channels, channel_unchecked }: ComponentDiagnosticActions & ChannelInventory & { row: ComponentVersionRow; hide_probe_error: boolean }) {
  const { t, locale, intl, formatText } = useLocale();
  const [expanded, setExpanded] = useState(false);
  const details_id = useId();
  const action = row.action;
  const action_label = action === "reconnect" ? t("about.reconnect") : t("about.restart");
  const probe_error = hide_probe_error ? null : row.error;
  const action_message = action_error?.component_id === row.component_id ? formatText(action_error.message) : null;
  const builds = componentBuildViews(row, locale);
  const difference = differingBuildIds(builds);
  const views = builds.map((view, index) => {
    const channels = componentBuildChannels(view.info, build_channels);
    const names = channels.map((channel) => channel === "stable" || channel === "nightly" ? t(`about.channel_${channel}`) : channel);
    const show_sha = channels.includes("nightly") || channels.includes("local");
    const revision = difference?.field === "source_revision" ? difference.values[index]
      : show_sha ? view.info?.source_revision?.slice(0, 8) : undefined;
    const fingerprint = difference?.field === "source_fingerprint" ? difference.values[index] : undefined;
    return { ...view, revision, fingerprint, channels: intl.formatList(names, { type: "unit" }) };
  });
  const mismatch = row.status === "incompatible" || views.some((view) =>
    componentProtocolLines(row, view, locale).some((line) => line.status === "incompatible"));
  const label = row.location === "local" ? row.label : row.component === "ctl_agent" ? "ctl-agent" : row.component;
  const status = row.observation === "not_checked" || row.observation === "installed" ? "unknown" : row.status;
  const show_details = t(expanded ? "about.hide_component_details" : "about.show_component_details", { name: row.label });

  return <tbody role="rowgroup">
    <tr role="row" className="about-service-row">
      <th role="rowheader" scope="row">
        <button type="button" className="about-service-toggle" aria-expanded={expanded} aria-controls={details_id}
          aria-label={show_details} onClick={() => setExpanded(!expanded)}>
          <Icon name={expanded ? "chevron_down" : "chevron_right"} size={14} />
          <strong>{label}</strong>
        </button>
        {row.observation === "last_observed" && <small className="about-service-observation">{t("about.active_connection")}</small>}
        {probe_error && <div title={probe_error}><InlineFeedback className="about-row-feedback">{row.location === "remote" ? t("about.check_component_failed") : probe_error}</InlineFeedback></div>}
        {action_message && <InlineFeedback className="about-row-feedback">{action_message}</InlineFeedback>}
      </th>
      <td role="cell" className="about-service-build">{views.map((view) => <div className="about-build-summary" key={view.label}>
        <span>{view.label}</span>
        <code>{view.info?.version ?? t("about.unknown")}{view.revision && <> · <span title={view.info?.source_revision ?? undefined}>{view.revision}</span></>}</code>
        {view.info && (view.channels
          ? <span className="about-build-channel" title={t("about.catalog_channel")}>{view.channels}</span>
          : <span>{t(channel_unchecked ? "about.channel_not_checked" : "about.channel_unknown")}</span>)}
        {view.fingerprint && <span title={view.info?.source_fingerprint ?? undefined}>{t("about.build_compact", { build: view.fingerprint })}</span>}
        {view.info?.dirty && <span>{t("about.modified")}</span>}
      </div>)}</td>
      <td role="cell" className="about-service-status"><div>
        {mismatch && <span className="about-version-status about-status-incompatible">{t("about.protocol_mismatch")}</span>}
        {row.restart_required && <span className="about-version-status about-status-outdated">{t(row.component === "ctl_agent" ? "about.reconnect_to_apply" : "about.restart_required")}</span>}
        {!mismatch && !row.restart_required && <span className={`about-version-status about-status-${status}`}>{statusLabel(row, locale)}</span>}
      </div></td>
      <td role="cell" className="about-service-action">{action && <button type="button" onClick={() => on_restart(row.component_id)} disabled={busy_id !== null}
        aria-label={t(action === "reconnect" ? "about.reconnect_component" : "about.restart_component", { name: row.label })}>
        {busy_id === row.component_id ? restarting ? action === "reconnect" ? t("about.reconnecting") : t("about.restarting") : t("about.checking") : action_label}
      </button>}</td>
    </tr>
    <tr role="row" id={details_id} hidden={!expanded} className="about-service-details"><td role="cell" colSpan={4}>{expanded && <>
      {row.detail && <p>{row.detail}</p>}
      {probe_error && row.location === "remote" && <p>{probe_error}</p>}
      <div className="about-build-details">{views.map((view) => <section key={view.label} aria-label={view.label}>
        <h4>{view.label}</h4>
        <dl>
          <div><dt>{t("about.service_version")}</dt><dd>{view.info?.version ?? t("about.not_reported")}</dd></div>
          <div><dt title={t("about.catalog_channel")}>{t("about.service_channel")}</dt><dd>{view.channels || t(channel_unchecked ? "about.not_checked" : "about.unknown")}</dd></div>
          <div><dt>{t("about.service_revision")}</dt><dd>{view.info?.source_revision ?? t("about.not_reported")}</dd></div>
          <div><dt>{t("about.service_fingerprint")}</dt><dd>{view.info?.source_fingerprint ?? t("about.not_reported")}</dd></div>
        </dl>
        {view.info?.dirty != null && <p>{t(view.info.dirty ? "about.modified_source" : "about.clean_source")}</p>}
        <ComponentProtocols row={row} view={view} />
      </section>)}</div>
    </>}</td></tr>
  </tbody>;
}

export function ComponentVersionTable({ group, ...actions }: ComponentDiagnosticActions & ChannelInventory & { group: ComponentHostGroup }) {
  const { t, locale } = useLocale();
  const failure = componentHostFailure(group.rows, locale);
  // Preserve native table navigation when the narrow layout stacks its cells.
  return <table role="table" className="about-version-table" aria-label={t("about.host_component_versions", { host: group.label })}>
    <thead role="rowgroup"><tr role="row">
      <th role="columnheader" scope="col">{t("about.component")}</th><th role="columnheader" scope="col">{t("about.build_and_channel")}</th>
      <th role="columnheader" scope="col">{t("about.status")}</th><th role="columnheader" scope="col">{t("about.actions")}</th>
    </tr></thead>
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
