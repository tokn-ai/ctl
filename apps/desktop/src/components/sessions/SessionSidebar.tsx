import { ControlPopover } from "../ui/ControlPopover";
import { useId, useState } from "react";
import { Icon } from "../ui/Icon";
import { SessionIcon, sessionIconLabel } from "./SessionIcon";
import { HostColorPicker } from "./HostColorPicker";
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
  connectionUnavailableLabel,
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
): { fullTitle: string; compactTitle: string } {
  const title = formatTerminalTitle(session, shellState);

  // `formatTerminalTitle` deliberately uses the session name as a general
  // fallback. In the sidebar, keep that stable identifier in the detail line
  // instead, so an unobserved shell remains visually neutral rather than
  // repeating the same `session-1` label twice.
  if (!shellState?.cwd) {
    const fullTitle = title.command ?? "Shell";
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

function hostTitle(target: ConnectionTarget): string {
  const lines = [targetLabel(target)];
  if (target.kind === "ssh" && target.remote_info) {
    lines.push(
      `Agent ${target.remote_info.agent_version}`,
      `Remote ID: ${target.remote_info.remote_id}`,
    );
    if (target.remote_info.bundle) {
      lines.push(
        `Bundle: ${target.remote_info.bundle.bundle_id}`,
        `Revision: ${target.remote_info.bundle.git_revision}`,
      );
    }
  }
  return lines.join("\n");
}

function hostConnectionPresentation(target: ConnectionTarget, connection: HostConnectionStatus | undefined): {
  state: string; label: string; route_unavailable?: boolean;
} {
  const observation = connection?.observation;
  const reachability = connection?.reachability;
  const connected = observation ? observation.availability === "available" : connection?.state === "connected";
  if (connected) return { state: "connected", label: "SSH connected" };
  if (reachability?.state === "available") return { state: "available", label: "SSH available" };
  if (target.kind === "ssh" && target.unavailable)
    return { state: "error", label: connectionUnavailableLabel(target), route_unavailable: true };
  if (reachability) {
    if (reachability.reason === "vpn_disconnected") return { state: "disconnected", label: "VPN disconnected" };
    if (reachability.state === "unavailable") return { state: "disconnected", label: "SSH unavailable" };
    if (reachability.state === "checking") return { state: "checking", label: "Checking SSH…" };
    if (reachability.state === "not_checked") return { state: "disconnected", label: "SSH not checked" };
    return { state: "unknown", label: "SSH status unknown" };
  }
  if (observation) {
    if (observation.availability === "unavailable") return {
      state: "disconnected", label: connection?.manually_disconnected ? "Disconnected manually" : "SSH disconnected",
    };
    if (observation.completeness === "pending") return { state: "checking", label: "Checking SSH…" };
    return { state: "unknown", label: "SSH status unknown" };
  }
  const state = connection?.state ?? "checking";
  return { state, label: {
    checking: "Checking…", connected: "SSH connected", connecting: "Connecting…",
    disconnecting: "Disconnecting…", disconnected: "Disconnected", error: "Connection error",
  }[state] };
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
    <aside className="session-sidebar" aria-label="ctmux sessions">
      <div className="sidebar-connections">
        <header className="sidebar-header">
          <strong>Hosts</strong>
          <div className="sidebar-header-actions">
            <ControlPopover label="Add or manage sessions" trigger_content={<Icon name="plus" />} placement="below" close_on_select>
              <button type="button" onClick={() => onNewShell()} disabled={creating}><Icon name="terminal" />New shell</button>
              <button type="button" onClick={onAddExisting} aria-label="Add existing session"><Icon name="plug" />Attach session</button>
              <button type="button" onClick={onAddHost}><Icon name="server" />Add host</button>
              {onChooseHost && <button type="button" onClick={onChooseHost} aria-label="Choose host to connect"><Icon name="plug" />Connect host</button>}
              {on_archives && <button type="button" onClick={on_archives} aria-label="Archived sessions"><Icon name="archive" />Archives</button>}
            </ControlPopover>
            <button
              className="icon-button"
              type="button"
              onClick={onRefresh}
              disabled={loading}
              aria-label="Refresh sessions"
              title="Refresh sessions"
            >
              <Icon name="refresh" />
            </button>
          </div>
        </header>

      </div>

      <div className="session-list">
        {loading && sessions.length === 0 ? (
          <p className="sidebar-state">Loading workspace…</p>
        ) : null}

        {taskSessions.length > 0 ? (
          <section className="session-group" aria-labelledby="session-group-tasks">
            <h3 id="session-group-tasks">
              Tasks <span>{taskSessions.length}</span>
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
                    aria-label={`${task.definition.name} — interactive task`}
                    title={task.definition.name}
                  >
                    <Icon name="tasks" class_name="session-icon" />
                    <span className="session-copy">
                      <strong>{task.definition.name}</strong>
                      <small>Interactive · {run.state}</small>
                    </span>
                  </button>
                  <div className="session-actions">
                    <button
                      className="session-action session-close"
                      type="button"
                      onClick={() => onStopTask?.(task)}
                      aria-label={`Stop ${task.definition.name}`}
                      title="Stop the task and its terminal"
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
          const unavailable = target.kind === "ssh" ? target.unavailable : undefined;
          const targetFailure = targetErrors.get(key);
          const targetError = targetFailure?.message;
          const hostError = targetError === unavailable ? undefined : targetError;
          const connection = target.kind === "ssh"
            ? hostConnections?.get(target.host_id ?? "")
            : undefined;
          const observation = connection?.observation;
          const reachability = connection?.reachability;
          const operation = connection?.operation;
          const presentation = hostConnectionPresentation(target, connection);
          const { state: connectionState, label: connectionLabel, route_unavailable: unavailableRoute } = presentation;
          const operationLabel = operation?.state === "pending" ? operation.kind === "connect" ? "Connecting…" : "Disconnecting…"
            : operation?.state === "failed" ? operation.kind === "connect" ? "Connect failed" : "Disconnect failed" : null;
          const connectionTitle = [
            connectionLabel,
            operationLabel,
            operation?.method_name,
            operation?.message,
            presentation.state === "connected" ? "The local SSH control connection is open. This check does not freshly verify remote responsiveness or terminal health." : null,
            reachability?.state === "available" ? "The endpoint answered with an SSH greeting. Authentication has not been checked." : null,
            observation?.checked_at_ms ? `SSH connection checked at ${new Date(observation.checked_at_ms).toLocaleTimeString()}` : null,
            reachability?.checked_at_ms ? `SSH reachability checked at ${new Date(reachability.checked_at_ms).toLocaleTimeString()}` : null,
            reachability?.method_names.length ? `Available methods: ${reachability.method_names.join(", ")}` : null,
            connection?.method_names.length
              ? `Connection methods: ${connection.method_names.join(", ")}`
              : null,
            unavailable,
            connection?.message !== unavailable ? connection?.message : null,
            observation?.completeness === "partial" ? "Some connection methods couldn't be checked." : null,
            observation?.failed_method_names.length ? `Status unknown for: ${observation.failed_method_names.join(", ")}` : null,
            reachability?.message,
          ].filter(Boolean).join("\n");
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
              <span className="host-group-name">{target.kind === "local" ? "Local" : targetLabel(target)}</span>
              {host?.source === "ssh_config" ? <span className="host-config-source" title="From SSH config; saved only when customized">SSH</span> : null}
              {host?.source === "tailscale" ? <span className="host-config-source" title="Discovered from Tailscale; saved only when customized">Tailscale</span> : null}
              {can_expand && <span className="host-group-count">{groupSessions.length}</span>}
            </span>
          </>;
          const heading_title = [hostTitle(target), hostError ? `Sessions: ${hostError}` : null].filter(Boolean).join("\n");
          return (
            <section
              className="session-group host-group"
              key={key}
              aria-label={`${targetLabel(target)} sessions`}
            >
              <div className={`host-group-header ${unavailableRoute ? "has-error" : ""}`}>
                {can_expand ? <button
                  className="host-group-toggle"
                  type="button"
                  aria-expanded={expanded}
                  aria-controls={childrenId}
                  aria-label={`${expanded ? "Collapse" : "Expand"} ${targetLabel(target)}`}
                  title={heading_title}
                  onClick={() => toggleHost(key)}
                >
                  {heading}
                </button> : <div className="host-group-label" title={heading_title}>{heading}</div>}
                <div className="host-color-control">
                  {target.kind === "local" ? <span className="host-color-fixed" title="Local · white" aria-hidden="true">
                    <span className="host-color-dot" style={{ background: hostColor(target) }} />
                  </span> : <HostColorPicker
                    host_name={targetLabel(target)}
                    color={hostColor(target, host_colors[host_id])}
                    default_color={hostColor(target)}
                    disabled={color_picker_disabled}
                    on_change={(color) => on_host_color_change?.(host_id, color)}
                  />}
                </div>
                {target.kind === "ssh" ? (
                  <span
                    className="host-connection-status"
                    role="status"
                    aria-label={`Host connection for ${targetLabel(target)}: ${connectionLabel}${operationLabel ? ` · ${operationLabel}` : ""}`}
                    data-state={connectionState}
                    data-availability={observation?.availability}
                    data-reachability={reachability?.state}
                    data-completeness={observation?.completeness}
                    data-operation={operation?.state}
                    title={connectionTitle}
                  >
                    <span className="host-connection-dot" aria-hidden="true" />
                    <span className="host-connection-label">{operationLabel ?? connectionLabel}</span>
                  </span>
                ) : null}
                {!can_expand && hostError ? <p
                  className="host-session-error"
                  role="status"
                  aria-label={`Sessions unavailable: ${hostError}`}
                  title={hostError}
                ><Icon name="info" size={12} /><span className="host-connection-label">Sessions unavailable</span></p> : null}
                <div className="host-group-actions">
                  <button
                    className="session-action"
                    type="button"
                    onClick={() => onNewShell(target)}
                    disabled={creating || Boolean(target.kind === "ssh" && target.unavailable)}
                    aria-label={`New shell on ${targetLabel(target)}`}
                    title={`New shell on ${targetLabel(target)}`}
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
                        aria-label={`Disconnect host ${targetLabel(target)}`}
                        title="Close the shared SSH connection and pause forwards. Remote sessions keep running."
                      >
                        <Icon name="unplug" size={14} />
                      </button>
                    ) : (
                      <button
                        className="session-action"
                        type="button"
                        onClick={() => onConnectHost(target)}
                        disabled={!(connectableHostKeys?.has(key) ?? !target.unavailable) || connectionBusy}
                        aria-label={`Connect to ${targetLabel(target)}`}
                        title={`Connect to ${hostTitle(target)}`}
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
                        aria-label={`Port forwarding for ${targetLabel(target)}`}
                        title={`Port forwarding for ${targetLabel(target)}`}
                      >
                        <Icon name="ports" size={14} />
                      </button>
                    ) : null}
                    {onHostSettings ? (
                      <button
                        className="session-action"
                        type="button"
                        onClick={() => onHostSettings(target)}
                        aria-label={`Host settings for ${targetLabel(target)}`}
                        title={`Host settings for ${targetLabel(target)}`}
                      >
                        <Icon name="settings" size={14} />
                      </button>
                    ) : null}
                  </> : null}
                </div>
              </div>
              {can_expand && <div className="host-group-children" id={childrenId} hidden={!expanded}>
                {groupSessions.map((session) => {
                  const identity = sessionKey(session);
                  const { fullTitle, compactTitle } = sidebarTitle(
                    session,
                    shellStates.get(identity) ?? null,
                  );
                  const selected = identity === selectedSessionKey;
                  const closing = closingSessionKeys.has(identity);
                  const disconnecting = identity === disconnectingSessionKey;
                  const canDisconnect = openTabSessionKeys.has(identity);
                  const attachment = attachmentStates?.get(identity);
                  const observed_status = session.status === "unknown"
                    ? "unverified"
                    : session.status;
                  const status = attachment?.phase === "error" && attachment.error_code === "automatic_reconnect_timeout"
                    ? "Reconnect failed"
                    : attachment ? attachmentPhaseLabel(attachment.phase)
                    : session.status === "running" ? "Last seen running" : observed_status;
                  const age = attachment?.phase === "attached" ? null : lastSeenAge(session.last_seen_at_ms, now_ms);
                  const observed_at = age !== null && isObservationTime(session.last_seen_at_ms)
                    ? new Date(session.last_seen_at_ms).toISOString() : null;
                  const observation_title = observed_at ? `Last observed by this app: ${observed_at}` : undefined;
                  const dimensions = attachment?.phase === "attached" || hasKnownTerminalSize(session)
                    ? ` · ${session.terminal_size.columns}×${session.terminal_size.rows}`
                    : "";
                  const details_title = [`${session.name} · ${status}${dimensions} · Session last reported ${observed_status}`, attachment?.message, observation_title].filter(Boolean).join("\n");
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
                        aria-label={`${fullTitle} — ${session.name}`}
                        aria-description={[sessionIconLabel(shellStates.get(identity)?.shell_type, session_layouts?.get(identity)), observation_title].filter(Boolean).join(". ")}
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
                          aria-label={`Remove ${session.name} from workspace`}
                          title="Remove from workspace; keep the shell running"
                        >
                          <Icon name="minus" size={14} />
                        </button>
                        {canDisconnect ? (
                          <button
                            className="session-action"
                            type="button"
                            onClick={() => onDisconnect(session)}
                            disabled={disconnecting || closing}
                            aria-label={`Disconnect from ${session.name}`}
                            title="Disconnect this tab; keep the session running"
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
                          aria-label={`Terminate ${session.name}`}
                          title="Terminate the session and its shell"
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
