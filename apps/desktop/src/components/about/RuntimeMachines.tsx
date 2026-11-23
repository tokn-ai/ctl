import { useCallback, useEffect, useId, useRef, useState } from "react";
import { groupComponentHosts, type ComponentHostGroup } from "../../features/about/componentVersions";
import { useLocale } from "../../features/i18n/LocaleContext";
import type { DisplayText } from "../../features/i18n/messages";
import { LOCAL_TARGET, supportsComponentUpdate, targetKey, targetLabel } from "../../features/targets/targets";
import { localizedErrorText } from "../../lib/errors";
import { cancelComponentUpdate, getAboutRuntimeStatus, syncAboutRuntime } from "../../lib/tauri";
import type { AboutRuntimeStatus, ComponentVersionRow, ConnectionTarget, SshConnectionTarget } from "../../lib/types";
import { InlineFeedback } from "../feedback/InlineFeedback";
import { Icon } from "../ui/Icon";
import { ComponentVersionTable, HostStatus, type ComponentDiagnosticActions } from "./ComponentVersionTable";

interface Actions extends ComponentDiagnosticActions {
  on_install(target: ConnectionTarget): void;
  on_inspect(target: SshConnectionTarget): void;
  on_synced(target: ConnectionTarget): void;
}

interface Props extends Actions {
  visible: boolean;
  targets: readonly ConnectionTarget[];
  rows: readonly ComponentVersionRow[];
  refresh_revision: ReadonlyMap<string, number>;
}

interface MachineProps extends Actions {
  visible: boolean;
  target?: ConnectionTarget;
  group: ComponentHostGroup;
  revision: number;
}

// Keep observations scoped to the chosen route and verified account across edits.
function routeKey(target: ConnectionTarget): string {
  if (target.kind === "local") return "local";
  const { remote_info, host_name: _, ...route } = target;
  return JSON.stringify([route, remote_info?.remote_id]);
}

function RuntimeMachine({ visible, group, target, revision, on_synced, on_install, on_inspect, ...actions }: MachineProps) {
  const { t, formatText, intl } = useLocale();
  const [expanded, setExpanded] = useState(false);
  const [status, setStatus] = useState<AboutRuntimeStatus | null>(null);
  const [checking, setChecking] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [readError, setReadError] = useState<DisplayText | null>(null);
  const [syncError, setSyncError] = useState<DisplayText | null>(null);
  const [outcome, setOutcome] = useState<"installed" | "pinned" | "current" | null>(null);
  const mounted = useRef(false);
  const generation = useRef(0);
  const pendingSync = useRef(false);
  const attempt = useRef<string | null>(null);
  const lastRevision = useRef(revision);
  const targetRef = useRef(target);
  targetRef.current = target;
  const detailsId = useId();
  const local = target?.kind === "local";
  const unavailable = target?.kind === "ssh" ? target.unavailable : undefined;
  const canManage = target && (local || !!target.host_id);
  const canInstall = target && supportsComponentUpdate(target);
  const canInspect = target?.kind === "ssh" && target.remote_platform !== "windows" &&
    !group.rows.some((row) => row.error_code === "remote_windows_maintenance_unsupported");
  const disabled = syncing || checking || actions.busy_id !== null || !!unavailable;

  const check = useCallback(async () => {
    const destination = targetRef.current;
    if (!destination || (destination.kind === "ssh" && destination.unavailable)) return;
    const request = ++generation.current;
    setChecking(true);
    setReadError(null);
    try {
      const next = await getAboutRuntimeStatus(destination);
      if (request === generation.current) setStatus(next);
    } catch (error) {
      if (request === generation.current) setReadError(localizedErrorText(error));
    } finally {
      if (request === generation.current) setChecking(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current++;
      if (attempt.current) void cancelComponentUpdate(attempt.current).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    if (!visible) return;
    const installed = revision !== lastRevision.current;
    lastRevision.current = revision;
    if (installed) {
      setOutcome(null);
      setSyncError(null);
    }
    if ((local || installed) && !pendingSync.current) void check();
  }, [visible, revision, local, check]);

  async function sync() {
    if (!target || disabled || pendingSync.current) return;
    const id = crypto.randomUUID();
    attempt.current = id;
    pendingSync.current = true;
    setSyncing(true);
    setStopping(false);
    setOutcome(null);
    setSyncError(null);
    try {
      const result = await syncAboutRuntime(target, id);
      if (mounted.current) setOutcome(result.state);
    } catch (error) {
      if (mounted.current) setSyncError(localizedErrorText(error));
    } finally {
      attempt.current = null;
      // A failed verification can follow successful activation. Read the actual selection.
      if (mounted.current) {
        on_synced(target);
        await check();
        if (mounted.current) setSyncing(false);
      }
      pendingSync.current = false;
    }
  }

  async function stop() {
    const id = attempt.current;
    if (!id || stopping) return;
    setStopping(true);
    try {
      await cancelComponentUpdate(id);
    } catch (error) {
      if (mounted.current && attempt.current === id) {
        setStopping(false);
        setSyncError(localizedErrorText(error));
      }
    }
  }

  const source = status?.origin?.channel;
  const sourceLabel = source === "stable" || source === "nightly" ? t(`about.channel_${source}`)
    : source ? t("about.runtime_source_pinned", { source }) : t("about.unknown");
  const resultLabel = outcome === "installed" ? "about.runtime_synced" :
    outcome === "pinned" ? "about.runtime_kept_pin" : "about.runtime_already_current";

  return <section className="runtime-machine" aria-label={group.label}>
    <div className="runtime-machine-main">
      <div className="runtime-machine-identity">
        <Icon name={local ? "monitor" : "server"} size={19} />
        <h3>{group.label}</h3>
      </div>
      {canManage && <div className="runtime-machine-installation" aria-live="polite" aria-busy={checking}>
        {status?.active ? <dl>
          <div><dt>{t("about.runtime_build")}</dt><dd title={status.active}>{status.build_label ?? status.origin?.publication_id ?? status.active}</dd></div>
          <div><dt>{t("about.runtime_source")}</dt><dd title={status.origin?.publication_id ?? undefined}>{sourceLabel}</dd></div>
        </dl> : <span className="about-muted">{checking ? t("about.checking") : status ? t("about.runtime_no_installation") : t("about.not_checked")}</span>}
        {local && !canInstall && status?.active === null && !checking && !readError &&
          <span>{intl.formatMessage({ id: "about.runtime_install_windows_x64" }, { command: <code>ctlup install --channel stable</code> })}</span>}
        {status?.missing && <span className="about-version-status about-status-outdated">{t("about.runtime_missing")}</span>}
      </div>}
      {canManage && <div className="runtime-machine-actions">
        <button type="button" disabled={disabled} onClick={() => { setOutcome(null); setSyncError(null); void check(); }}>{t(checking ? "about.checking" : "about.runtime_check")}</button>
        <button type="button" className="runtime-sync" disabled={disabled || (status !== null && !status.active)} onClick={() => void sync()}><Icon name="refresh" size={14} />{t(syncing ? "about.runtime_syncing" : "about.runtime_sync")}</button>
        {syncing && attempt.current && <button type="button" disabled={stopping} onClick={() => void stop()}><Icon name="stop" size={14} />{t(stopping ? "about.update_stopping" : "about.update_stop")}</button>}
        {canInstall && <button type="button" disabled={disabled} onClick={() => on_install(target)}>{t("about.runtime_install_from")}</button>}
      </div>}
    </div>
    {unavailable && <InlineFeedback severity="warning">{unavailable}</InlineFeedback>}
    {readError && <InlineFeedback>{status && <>{t("about.runtime_status_failed_cached")} </>}{formatText(readError)}</InlineFeedback>}
    {syncError && <InlineFeedback>{formatText(syncError)}</InlineFeedback>}
    {outcome && <InlineFeedback severity="success">{t(resultLabel)}</InlineFeedback>}
    {(group.rows.length > 0 || canInspect) && <div className="runtime-machine-diagnostics">
      <button type="button" className="runtime-details-toggle" onClick={() => setExpanded(!expanded)} aria-expanded={expanded} aria-controls={detailsId}
        aria-label={t(expanded ? "about.hide_host_components" : "about.show_host_components", { host: group.label })}>
        <Icon name={expanded ? "chevron_down" : "chevron_right"} size={14} />{t("about.components")}
      </button>
      {group.rows.length > 0 && <HostStatus group={group} action_error={actions.action_error} />}
      <div id={detailsId} hidden={!expanded} className="runtime-machine-details">
        {canInspect && <button type="button" disabled={disabled} className="runtime-check-host" aria-label={t("about.check_named_host", { host: group.label })} onClick={() => on_inspect(target)}>{t("about.check_host")}</button>}
        {expanded && group.rows.length > 0 && <div className="about-table-scroll"><ComponentVersionTable group={group} {...actions} /></div>}
      </div>
    </div>}
  </section>;
}

export function RuntimeMachines({ visible, targets, rows, refresh_revision, ...actions }: Props) {
  const { t, locale } = useLocale();
  const groups = groupComponentHosts(rows.filter((row) => row.component !== "ctmux"), locale);
  const machines = new Map<string, { group: ComponentHostGroup; target?: ConnectionTarget }>();
  for (const target of [LOCAL_TARGET, ...targets]) {
    const key = targetKey(target);
    if (machines.has(key) || (target.kind === "ssh" && !target.host_id)) continue;
    const local = target.kind === "local";
    const group = groups.find((item) => local ? item.key === "local" : item.host_id === target.host_id);
    const label = local ? t("about.this_computer") : target.host_name ?? group?.label ?? targetLabel(target, locale);
    machines.set(key, { target, group: { key: group?.key ?? key, label, host_id: local ? null : target.host_id ?? null, rows: group?.rows ?? [] } });
  }
  // Active transports can contribute diagnostics for accounts that have no saved route.
  for (const group of groups) {
    if (group.key === "local" || group.host_id && machines.has(`host:${group.host_id}`)) continue;
    machines.set(group.key, { group });
  }
  return <section className="about-section runtime-machines" aria-label={t("about.runtime_machines")}>
    <h2>{t("about.runtime_machines")}</h2>
    <p className="about-muted">{t("about.runtime_sync_help")}</p>
    <div className="runtime-machine-list">{[...machines].map(([key, { target, group }]) =>
      <RuntimeMachine key={target ? routeKey(target) : key} target={target} group={group}
        visible={visible} revision={refresh_revision.get(key) ?? 0} {...actions} />)}</div>
    {machines.size === 1 && rows.length > 0 && <p className="about-muted runtime-no-hosts">{t("about.no_remote_hosts")}</p>}
  </section>;
}
