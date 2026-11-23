import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { runtimeChannelShowsRevision, runtimeTargetLabel } from "../../features/about/runtimeTargets";
import "./machines.css";
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
  service_refresh?: ReactNode;
  local_builds?: ReactNode;
  on_local_status?(status: AboutRuntimeStatus | null, loading: boolean, error: DisplayText | null): void;
  visible: boolean;
  targets: readonly ConnectionTarget[];
  rows: readonly ComponentVersionRow[];
  refresh_revision: ReadonlyMap<string, number>;
}

interface MachineProps extends Actions, Pick<Props, "on_local_status"> {
  selected: boolean;
  on_select(): void;
  detail_mount: HTMLDivElement | null;
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

function RuntimeMachine({ visible, group, target, revision, selected, on_select, detail_mount, on_local_status, on_synced, on_install, on_inspect, ...actions }: MachineProps) {
  const { t, locale, formatText, intl } = useLocale();
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
  const reportLocal = useRef(on_local_status);
  reportLocal.current = on_local_status;
  useEffect(() => {
    if (local) reportLocal.current?.(status, checking, readError);
  }, [local, status, checking, readError]);
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

  const lastSeen = target?.kind === "ssh" ? target.remote_info?.bundle : undefined;
  const inspectedTarget = group.rows.find((row) => row.observation === "installed" && row.observed_agent_target)?.observed_agent_target;
  const observedTargets = new Set(group.rows.flatMap((row) => row.observed_agent_target ? [row.observed_agent_target] : []));
  const recordedTarget = inspectedTarget ?? lastSeen?.target_triple ?? (observedTargets.size === 1 ? [...observedTargets][0] : undefined);
  const triple = status?.target_triple ?? recordedTarget;
  const platform = triple ? runtimeTargetLabel(triple, locale)
    : target?.kind === "ssh" && target.remote_platform === "windows" ? "Windows" : t("about.unknown_target");
  const platformCaption = !status?.target_triple && recordedTarget ? t("about.machine_last_seen", { value: platform }) : platform;
  const build = status?.active_build;
  const showRevision = !!source && runtimeChannelShowsRevision(source);
  const activeVersion = build ? [build.version, showRevision && (build.source_revision?.slice(0, 8) ?? t("about.desktop_sha_unavailable")), build.dirty && t("about.modified")].filter(Boolean).join(" · ")
    : status?.build_label ?? status?.origin?.publication_id ?? status?.active;
  const storedVersion = status?.active ? activeVersion : status ? t("about.runtime_no_installation") : null;
  const choiceVersion = storedVersion ?? (lastSeen && !status ? t("about.machine_last_seen", { value: lastSeen.app_version }) : t("about.not_checked"));

  // Each route owns its operations while the detail pane follows the selected machine.
  return <li>
    <button type="button" className="machine-choice" aria-label={group.label} aria-pressed={selected}
      aria-controls={selected ? detailsId : undefined} aria-describedby={`${detailsId}-platform ${detailsId}-version`} onClick={on_select}>
      <Icon name={local ? "monitor" : "server"} size={16} />
      <span className="machine-choice-text"><strong>{group.label}</strong>
        <span id={`${detailsId}-platform`} title={triple}>{platformCaption}</span>
        <span id={`${detailsId}-version`}>{syncing ? t("about.runtime_syncing") : choiceVersion}</span>
      </span>
    </button>
    {selected && detail_mount && createPortal(<div id={detailsId}>
      <header className="machine-inspector-heading">
        <div><h3>{group.label}</h3>
          <span className="machine-platform" title={triple}>{platformCaption}</span>
          {triple && triple !== platform && <code className="machine-triple">{triple}</code>}
        </div>
        {canManage && <div className="machine-actions">
          <button type="button" disabled={disabled} onClick={() => { setOutcome(null); setSyncError(null); void check(); }}>{t(checking ? "about.checking" : "about.runtime_check")}</button>
          <button type="button" className="runtime-sync" disabled={disabled || (status !== null && !status.active)} onClick={() => void sync()}><Icon name="refresh" size={14} />{t(syncing ? "about.runtime_syncing" : "about.runtime_sync")}</button>
          {syncing && attempt.current && <button type="button" disabled={stopping} onClick={() => void stop()}><Icon name="stop" size={14} />{t(stopping ? "about.update_stopping" : "about.update_stop")}</button>}
          {canInstall && <button type="button" disabled={disabled} onClick={() => on_install(target)}>{t("about.runtime_install_from")}</button>}
        </div>}
      </header>
      {canManage && <div className="machine-installed" aria-live="polite" aria-busy={checking}>
        <dl>
          <div><dt>{t("about.machine_installed_bundle")}</dt><dd title={status?.active ?? undefined}>{storedVersion ?? t(checking ? "about.checking" : "about.not_checked")}{status?.active && showRevision && !build && <small className="machine-revision-unknown">{t("about.desktop_sha_unavailable")}</small>}</dd></div>
          {status?.active && <div><dt>{t("about.runtime_source")}</dt><dd title={status.origin?.publication_id ?? undefined}>{sourceLabel}</dd></div>}
        </dl>
        {local && !canInstall && status?.active === null && !checking && !readError &&
          <span>{intl.formatMessage({ id: "about.runtime_install_windows_x64" }, { command: <code>ctlup install --channel stable</code> })}</span>}
        {status?.missing && <span className="about-version-status about-status-outdated">{t("about.runtime_missing")}</span>}
      </div>}
      {unavailable && <InlineFeedback severity="warning">{unavailable}</InlineFeedback>}
      {readError && <InlineFeedback>{status && <>{t("about.runtime_status_failed_cached")} </>}{formatText(readError)}</InlineFeedback>}
      {syncError && <InlineFeedback>{formatText(syncError)}</InlineFeedback>}
      {outcome && <InlineFeedback severity="success">{t(resultLabel)}</InlineFeedback>}
      <div className="machine-services-heading">
        <h4>{t("about.machine_services")}</h4>
        {canInspect && <button type="button" disabled={disabled} aria-label={t("about.check_named_host", { host: group.label })} onClick={() => on_inspect(target)}>{t("about.check_host")}</button>}
      </div>
      {group.rows.length > 0 ? <>
        <HostStatus group={group} action_error={actions.action_error} />
        <div className="about-table-scroll machine-services"><ComponentVersionTable group={group} build_channels={status?.build_channels}
          active_build={build} channel_unchecked={!!canManage && !status && !readError} {...actions} /></div>
      </> : <p className="about-muted">{t(target?.kind === "ssh" && target.remote_platform === "windows" ? "about.check_windows" : "about.machine_no_service_observations")}</p>}
    </div>, detail_mount)}
  </li>;
}

export function RuntimeMachines({ visible, targets, rows, refresh_revision, service_refresh, local_builds, ...actions }: Props) {
  const [selectedKey, setSelectedKey] = useState(targetKey(LOCAL_TARGET));
  const [detailMount, setDetailMount] = useState<HTMLDivElement | null>(null);
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
  const currentKey = machines.has(selectedKey) ? selectedKey : targetKey(LOCAL_TARGET);
  return <section className="about-section runtime-machines" aria-label={t("about.runtime_machines")}>
    <div className="machine-section-heading"><h2>{t("about.runtime_machines")}</h2>{service_refresh}</div>
    <div className="machine-workbench">
      <ul className="machine-selector" aria-label={t("about.runtime_machines")}>{[...machines].map(([key, { target, group }]) =>
        <RuntimeMachine key={target ? routeKey(target) : key} target={target} group={group}
          selected={currentKey === key} on_select={() => setSelectedKey(key)} detail_mount={detailMount}
          visible={visible} revision={refresh_revision.get(key) ?? 0} {...actions} />)}</ul>
      <section className="machine-inspector" aria-label={machines.get(currentKey)?.group.label}>
        <div ref={setDetailMount} />
        {local_builds && <div className="machine-local-builds" hidden={currentKey !== targetKey(LOCAL_TARGET)}>{local_builds}</div>}
      </section>
    </div>
    {machines.size === 1 && rows.length > 0 && <p className="about-muted runtime-no-hosts">{t("about.no_remote_hosts")}</p>}
  </section>;
}
