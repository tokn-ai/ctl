import { translate, type Locale } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { ControlPopover } from "../ui/ControlPopover";
import { useId, useState } from "react";
import { Icon } from "../ui/Icon";
import { SessionIcon, sessionIconLabel } from "./SessionIcon";
import { HostColorPicker } from "./HostColorPicker";
import { HostDetailsCard } from "./HostDetailsCard";
import { InlineFeedback } from "../feedback/InlineFeedback";
import { hostPresentation } from "../../features/workspace/hostPresentation";
import { hostColor } from "../../features/workspace/hostColor";
import { attachmentPhaseLabel } from "../../features/attachment/attachmentState";
import { isObservationTime, lastSeenAge, useLastSeenClock } from "../../features/sessions/lastSeen";
import { hasKnownTerminalSize } from "../../features/sessions/sessionObservation";
import type { ErrorDetails } from "../../lib/errors";
import {
  compactTerminalTitle,
  compactTerminalTitleParts,
  formatTerminalTitle,
} from "../../features/tabs/terminalTitle";
import type {
  ConnectionTarget,
  AttachmentViewState,
  HostConnectionStatus,
  ManagedTask,
  SessionSummary,
  ShellStateSummary,
  WorkspaceHost,
  ViewLayout,
} from "../../lib/types";
import {
  sessionKey,
  targetKey,
  targetLabel,
} from "../../features/targets/targets";

const SIDEBAR_TERMINAL_TITLE_MAX_LENGTH = 20;

interface SessionSidebarProps {
  targets: readonly ConnectionTarget[];
  hosts?: readonly WorkspaceHost[];
  host_colors?: Readonly<Record<string, string>>;
  color_picker_disabled?: boolean;
  on_host_color_change?(host_id: string, color: string | undefined): void;
  /** Hosts with at least one available saved connection method. */
  connectableHostKeys?: ReadonlySet<string>;
  targetErrors: ReadonlyMap<string, ErrorDetails>;
  hostConnections?: ReadonlyMap<string, HostConnectionStatus>;
  attachmentStates?: ReadonlyMap<string, AttachmentViewState>;
  sessions: SessionSummary[];
  interactiveTasks?: ManagedTask[];
  shellStates: ReadonlyMap<string, ShellStateSummary>;
  session_layouts?: ReadonlyMap<string, ViewLayout>;
  selectedSessionKey: string | null;
  openTabSessionKeys: ReadonlySet<string>;
  loading: boolean;
  list_error?: string | null;
  on_retry?(): void;
  creating: boolean;
  closingSessionKeys: ReadonlySet<string>;
  disconnectingSessionKey: string | null;
  onRefresh(): void;
  on_archives?(): void;
  onSelect(session: SessionSummary): void;
  onNewShell(target?: ConnectionTarget): void;
  onDisconnect(session: SessionSummary): void;
  onRequestClose(session: SessionSummary): void;
  onAddHost(): void;
  onChooseHost?(): void;
  onHostSettings?(target: ConnectionTarget): void;
  onConnectHost(target: ConnectionTarget): void;
  onDisconnectHost?(target: ConnectionTarget): void;
  onPortForward?(target: ConnectionTarget): void;
  onAddExisting(): void;
  onForget(session: SessionSummary): void;
  onSelectTask?(task: ManagedTask): void;
  onStopTask?(task: ManagedTask): void;
}

function sidebarTitle(
  session: SessionSummary,
  shellState: ShellStateSummary | null,
  locale: Locale,
): { fullTitle: string; compactTitle: string } {
  const title = formatTerminalTitle(session, shellState);

  // `formatTerminalTitle` deliberately uses the session name as a general
  // fallback. In the sidebar, keep that stable identifier in the detail line
  // instead, so an unobserved shell remains visually neutral rather than
  // repeating the same `session-1` label twice.
  if (!shellState?.cwd) {
    const fullTitle = title.command ?? translate(locale, "sidebar.shell");
    return {
      fullTitle,
      compactTitle: compactTerminalTitle(
        fullTitle,
        SIDEBAR_TERMINAL_TITLE_MAX_LENGTH,
      ),
    };
  }
  return {
    fullTitle: title.text,
    compactTitle: compactTerminalTitleParts(
      title,
      SIDEBAR_TERMINAL_TITLE_MAX_LENGTH,
    ),
  };
}

function hostTitle(target: ConnectionTarget, locale: Locale): string {
  const lines = [targetLabel(target, locale)];
  if (target.kind === "ssh" && target.remote_info) {
    lines.push(
      translate(locale, "sidebar.agent", { version: target.remote_info.agent_version }),
      translate(locale, "sidebar.remote_id", { id: target.remote_info.remote_id }),
    );
    if (target.remote_info.bundle) {
      lines.push(
        translate(locale, "sidebar.bundle", { bundle: target.remote_info.bundle.bundle_id }),
        translate(locale, "sidebar.revision", { revision: target.remote_info.bundle.git_revision }),
      );
    }
  }
  return lines.join("\n");
}

export function SessionSidebar({
  targets,
  hosts = [],
  host_colors = {},
  color_picker_disabled = false,
  on_host_color_change,
  connectableHostKeys,
  targetErrors,
  hostConnections,
  attachmentStates,
  sessions,
  interactiveTasks = [],
  shellStates,
  session_layouts,
  selectedSessionKey,
  openTabSessionKeys,
  loading,
  list_error,
  on_retry,
  creating,
  closingSessionKeys,
  disconnectingSessionKey,
  onRefresh,
  on_archives,
  onSelect,
  onNewShell,
  onDisconnect,
  onRequestClose,
  onAddHost,
  onChooseHost,
  onHostSettings,
  onConnectHost,
  onDisconnectHost,
  onPortForward,
  onAddExisting,
  onForget,
  onSelectTask,
  onStopTask,
}: SessionSidebarProps) {
  const { t, locale, formatText } = useLocale();
  const groupId = useId();
  const [collapsedHosts, setCollapsedHosts] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const taskSessions = interactiveTasks.filter(
    (task) =>
      task.definition.execution_mode === "interactive" &&
      !!task.active_run?.interactive?.session_id &&
      !task.active_run.interactive.released,
  );
  const taskSessionIds = new Set(
    taskSessions.map((task) => task.active_run!.interactive!.session_id),
  );
  const ordinarySessions = sessions.filter(
    (session) =>
      session.target.kind !== "local" || !taskSessionIds.has(session.session_id),
  );
  const now_ms = useLastSeenClock(ordinarySessions.some((session) =>
    isObservationTime(session.last_seen_at_ms) && attachmentStates?.get(sessionKey(session))?.phase !== "attached",
  ));
  const sessionGroups = targets.map((target) => ({
    target,
    sessions: ordinarySessions.filter(
      (session) => targetKey(session.target) === targetKey(target),
    ),
  }));

  function toggleHost(key: string) {
    setCollapsedHosts((current) => {
      const next = new Set(current);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }

  return (
    <aside className="session-sidebar" aria-label={t("sidebar.sessions_panel")}>
      <div className="sidebar-connections">
        <header className="sidebar-header">
          <strong>{t("sidebar.hosts")}</strong>
          <div className="sidebar-header-actions">
            <ControlPopover label={t("sidebar.manage_sessions")} trigger_content={<Icon name="plus" />} placement="below" close_on_select>
              <button type="button" onClick={() => onNewShell()} disabled={creating}><Icon name="terminal" />{t("sidebar.new_shell")}</button>
              <button type="button" onClick={onAddExisting} aria-label={t("sidebar.add_existing")}><Icon name="plug" />{t("sidebar.attach")}</button>
              <button type="button" onClick={onAddHost}><Icon name="server" />{t("sidebar.add_host")}</button>
              {onChooseHost && <button type="button" onClick={onChooseHost} aria-label={t("sidebar.choose_host")}><Icon name="plug" />{t("sidebar.connect_host")}</button>}
              {on_archives && <button type="button" onClick={on_archives} aria-label={t("sidebar.archives_label")}><Icon name="archive" />{t("sidebar.archives")}</button>}
            </ControlPopover>
            <button
              className="icon-button"
              type="button"
              onClick={onRefresh}
              disabled={loading}
              aria-label={t("sidebar.refresh")}
              title={t("sidebar.refresh")}
            >
              <Icon name="refresh" />
            </button>
          </div>
        </header>

      </div>

      <div className="session-list">
        {list_error ? <InlineFeedback action={{ label: t("sidebar.retry_sessions"), on_click: on_retry ?? onRefresh, disabled: loading }}>
          {t("sidebar.refresh_failed", { detail: list_error, cached: String(sessions.length > 0) })}
        </InlineFeedback> : null}
        {loading && sessions.length === 0 ? (
          <p className="sidebar-state">{t("sidebar.loading")}</p>
        ) : null}

        {taskSessions.length > 0 ? (
          <section className="session-group" aria-labelledby="session-group-tasks">
            <h3 id="session-group-tasks">
              {t("sidebar.tasks")} <span>{taskSessions.length}</span>
            </h3>
            {taskSessions.map((task) => {
              const run = task.active_run!;
              const selected =
                selectedSessionKey === `task:local:${task.task_id}`;
              return (
                <div
                  className={`session-row task-session-row ${selected ? "active" : ""}`}
                  key={task.task_id}
                >
                  <button
                    className="session-select"
                    type="button"
                    onClick={() => onSelectTask?.(task)}
                    aria-current={selected ? "true" : undefined}
                    aria-label={t("sidebar.interactive_task", { name: task.definition.name })}
                    title={task.definition.name}
                  >
                    <Icon name="tasks" class_name="session-icon" />
                    <span className="session-copy">
                      <strong>{task.definition.name}</strong>
                      <small>{t("sidebar.task_status", { status: t(`tasks.state_${run.state}`) })}</small>
                    </span>
                  </button>
                  <div className="session-actions">
                    <button
                      className="session-action session-close"
                      type="button"
                      onClick={() => onStopTask?.(task)}
                      aria-label={t("sidebar.stop_task", { name: task.definition.name })}
                      title={t("sidebar.stop_task_hint")}
                    >
                      <Icon name="stop" size={14} />
                    </button>
                  </div>
                </div>
              );
            })}
          </section>
        ) : null}
        {sessionGroups.map(({ target, sessions: groupSessions }) => {
          const key = targetKey(target);
          const host_id = target.kind === "local" ? "local" : target.host_id ?? key;
          const can_expand = groupSessions.length > 0;
          const expanded = can_expand && !collapsedHosts.has(key);
          const childrenId = `${groupId}-${encodeURIComponent(key)}`;
          const host = hosts.find((item) => item.host_id === (target.kind === "local" ? "local" : target.host_id));
          const connection = target.kind === "ssh"
            ? hostConnections?.get(target.host_id ?? "")
            : undefined;
          const operation = connection?.operation;
          const presentation = hostPresentation(target, connection, targetErrors.get(key), locale);
          const host_error = targetErrors.get(key);
          const details_description = [presentation.description,
            ...presentation.details.map((detail) => t("sidebar.detail", { label: detail.label, value: detail.value })),
          ].join("\n");
          const connectionBusy = connection?.state === "connecting" ||
            connection?.state === "disconnecting" || operation?.state === "pending";
          const showDisconnect = onDisconnectHost && (
            connection?.state === "connected" ||
            connection?.state === "disconnecting" ||
            (connection?.state === "error" && connection.method_names.length > 0)
          );
          const heading = <>
            {can_expand ? <Icon name={expanded ? "chevron_down" : "chevron_right"} size={12} />
              : <span className="host-chevron-spacer" aria-hidden="true" />}
            <span aria-hidden="true" />
            <span className="host-group-title">
              <span
                className="host-group-name"
                role={target.kind === "ssh" ? "status" : undefined}
                aria-label={target.kind === "ssh" ? t("sidebar.host_status", { host: targetLabel(target, locale), status: presentation.label }) : undefined}
                aria-description={details_description}
                data-tone={presentation.tone}
                data-connected={presentation.connected}
              >{target.kind === "local" ? t("sidebar.local") : targetLabel(target, locale)}</span>
              {host?.source === "ssh_config" ? <span className="host-config-source" title={t("sidebar.ssh_config_source")}>SSH</span> : null}
              {host?.source === "tailscale" ? <span className="host-config-source" title={t("sidebar.tailscale_source")}>Tailscale</span> : null}
              {can_expand && <span className="host-group-count">{groupSessions.length}</span>}
            </span>
          </>;
          return (
            <section
              className="session-group host-group"
              key={key}
              aria-label={t("sidebar.host_sessions", { host: targetLabel(target, locale) })}
            >
              <div className="host-group-header">
                <HostDetailsCard
                  host_name={target.kind === "local" ? t("sidebar.local") : targetLabel(target, locale)}
                  label={presentation.label}
                  tone={presentation.tone}
                  details={presentation.details}
                  description={presentation.description}
                >{(trigger_props) => can_expand ? <button
                  {...trigger_props}
                  className="host-group-toggle"
                  type="button"
                  aria-expanded={expanded}
                  aria-controls={childrenId}
                  aria-label={t("sidebar.toggle_host", { expanded: String(expanded), host: targetLabel(target, locale) })}
                  aria-description={details_description}
                  onClick={() => toggleHost(key)}
                >
                  {heading}
                </button> : <div
                  {...trigger_props}
                  className="host-group-label"
                  tabIndex={0}
                  aria-label={target.kind === "local" ? t("sidebar.local") : targetLabel(target, locale)}
                  aria-description={details_description}
                >{heading}</div>}</HostDetailsCard>
                <div className="host-color-control">
                  {target.kind === "local" ? <span className="host-color-fixed" title={t("sidebar.local_color")} aria-hidden="true">
                    <span className="host-color-dot" style={{ background: hostColor(target) }} />
                  </span> : <HostColorPicker
                    host_name={targetLabel(target, locale)}
                    color={hostColor(target, host_colors[host_id])}
                    connected={presentation.connected}
                    default_color={hostColor(target)}
                    disabled={color_picker_disabled}
                    on_change={(color) => on_host_color_change?.(host_id, color)}
                  />}
                </div>
                <div className="host-group-actions">
                  <button
                    className="session-action"
                    type="button"
                    onClick={() => onNewShell(target)}
                    disabled={creating || Boolean(target.kind === "ssh" && target.unavailable)}
                    aria-label={t("sidebar.new_on_host", { host: targetLabel(target, locale) })}
                    title={t("sidebar.new_on_host", { host: targetLabel(target, locale) })}
                  >
                    <Icon name="plus" size={14} />
                  </button>
                  {target.kind === "ssh" ? <>
                    {showDisconnect ? (
                      <button
                        className="session-action"
                        type="button"
                        onClick={() => onDisconnectHost?.(target)}
                        disabled={connectionBusy}
                        aria-label={t("sidebar.disconnect_host", { host: targetLabel(target, locale) })}
                        title={t("sidebar.disconnect_host_hint")}
                      >
                        <Icon name="unplug" size={14} />
                      </button>
                    ) : (
                      <button
                        className="session-action"
                        type="button"
                        onClick={() => onConnectHost(target)}
                        disabled={!(connectableHostKeys?.has(key) ?? !target.unavailable) || connectionBusy}
                        aria-label={t("sidebar.connect_to", { host: targetLabel(target, locale) })}
                        title={t("sidebar.connect_to", { host: hostTitle(target, locale) })}
                      >
                        <Icon name="plug" size={14} />
                      </button>
                    )}
                    {onPortForward ? (
                      <button
                        className="session-action"
                        type="button"
                        onClick={() => onPortForward(target)}
                        disabled={Boolean(target.unavailable)}
                        aria-label={t("sidebar.forwards_for", { host: targetLabel(target, locale) })}
                        title={t("sidebar.forwards_for", { host: targetLabel(target, locale) })}
                      >
                        <Icon name="ports" size={14} />
                      </button>
                    ) : null}
                    {onHostSettings ? (
                      <button
                        className="session-action"
                        type="button"
                        onClick={() => onHostSettings(target)}
                        aria-label={t("sidebar.settings_for", { host: targetLabel(target, locale) })}
                        title={t("sidebar.settings_for", { host: targetLabel(target, locale) })}
                      >
                        <Icon name="settings" size={14} />
                      </button>
                    ) : null}
                  </> : null}
                </div>
              </div>
              {host_error ? <InlineFeedback action={{ label: t("sidebar.retry_host"), on_click: on_retry ?? onRefresh, disabled: loading }}>
                {t("sidebar.host_failed", { detail: formatText(host_error.message), cached: String(groupSessions.length > 0) })}
              </InlineFeedback> : null}
              {can_expand && <div className="host-group-children" id={childrenId} hidden={!expanded}>
                {groupSessions.map((session) => {
                  const identity = sessionKey(session);
                  const { fullTitle, compactTitle } = sidebarTitle(
                    session,
                    shellStates.get(identity) ?? null,
                    locale,
                  );
                  const selected = identity === selectedSessionKey;
                  const closing = closingSessionKeys.has(identity);
                  const disconnecting = identity === disconnectingSessionKey;
                  const canDisconnect = openTabSessionKeys.has(identity);
                  const attachment = attachmentStates?.get(identity);
                  const observed_status = t(`sidebar.observed_${session.status}`);
                  const status = attachment?.phase === "error" && attachment.error_code === "automatic_reconnect_timeout"
                    ? t("sidebar.reconnect_failed")
                    : attachment ? attachmentPhaseLabel(attachment.phase, locale)
                    : session.status === "running" ? t("sidebar.last_running") : observed_status;
                  const age = attachment?.phase === "attached" ? null : lastSeenAge(session.last_seen_at_ms, now_ms, locale);
                  const observed_at = age !== null && isObservationTime(session.last_seen_at_ms)
                    ? new Date(session.last_seen_at_ms).toISOString() : null;
                  const observation_title = observed_at ? t("sidebar.observed_at", { time: observed_at }) : undefined;
                  const dimensions = attachment?.phase === "attached" || hasKnownTerminalSize(session)
                    ? ` · ${session.terminal_size.columns}×${session.terminal_size.rows}`
                    : "";
                  const details_title = [t("sidebar.observed_status", { name: session.name, status: status, dimensions: dimensions, observed: observed_status }), attachment?.message ? formatText(attachment.message) : null, observation_title].filter(Boolean).join("\n");
                  return (
                    <div
                      className={`session-row ${selected ? "active" : ""}`}
                      key={identity}
                    >
                      <button
                        className="session-select"
                        type="button"
                        onClick={() => onSelect(session)}
                        disabled={closing}
                        aria-current={selected ? "true" : undefined}
                        aria-label={t("sidebar.session_label", { title: fullTitle, name: session.name })}
                        aria-description={[sessionIconLabel(shellStates.get(identity)?.shell_type, session_layouts?.get(identity), locale), observation_title].filter(Boolean).join(". ")}
                        title={fullTitle}
                      >
                        <span aria-label={status} title={details_title}><SessionIcon
                          shell_type={shellStates.get(identity)?.shell_type}
                          layout={session_layouts?.get(identity)}
                          class_name="session-icon"
                        /></span>
                        <span className="session-copy">
                          <strong>{compactTitle}</strong>
                          {attachment?.phase !== "attached" && <small className="session-details" title={details_title}>
                            <span className="session-detail-label">
                              <span className="session-status" data-status={attachment?.phase ?? (session.status === "running" ? "unknown" : session.status)}>
                                {status}
                              </span>
                            </span>
                            {age !== null ? <span className="session-age">
                              <span aria-hidden="true"> · </span>
                              <time dateTime={observed_at ?? undefined} title={observation_title}>{age}</time>
                            </span> : null}
                          </small>}
                        </span>
                      </button>
                      <div className="session-actions">
                        <button
                          className="session-action"
                          type="button"
                          onClick={() => onForget(session)}
                          disabled={closing || disconnecting}
                          aria-label={t("sidebar.forget_session", { name: session.name })}
                          title={t("sidebar.forget_hint")}
                        >
                          <Icon name="minus" size={14} />
                        </button>
                        {canDisconnect ? (
                          <button
                            className="session-action"
                            type="button"
                            onClick={() => onDisconnect(session)}
                            disabled={disconnecting || closing}
                            aria-label={t("sidebar.disconnect_session", { name: session.name })}
                            title={t("sidebar.disconnect_session_hint")}
                          >
                            {disconnecting ? (
                              <span aria-hidden="true">…</span>
                            ) : (
                              <Icon name="unplug" size={14} />
                            )}
                          </button>
                        ) : null}
                        <button
                          className="session-action session-close"
                          type="button"
                          onClick={() => onRequestClose(session)}
                          disabled={closing || disconnecting}
                          aria-label={t("sidebar.terminate_session", { name: session.name })}
                          title={t("sidebar.terminate_session_hint")}
                        >
                          {closing ? (
                            <span aria-hidden="true">…</span>
                          ) : (
                            <Icon name="stop" size={14} />
                          )}
                        </button>
                      </div>
                    </div>
                  );
                })}
              </div>}
            </section>
          );
        })}
      </div>

    </aside>
  );
}
