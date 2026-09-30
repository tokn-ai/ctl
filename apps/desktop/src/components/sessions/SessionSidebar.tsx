import { useId, useState } from "react";
import { Icon } from "../ui/Icon";
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
  /** Hosts with at least one available saved connection method. */
  connectableHostKeys?: ReadonlySet<string>;
  targetErrors: ReadonlyMap<string, ErrorDetails>;
  hostConnections?: ReadonlyMap<string, HostConnectionStatus>;
  attachmentStates?: ReadonlyMap<string, AttachmentViewState>;
  sessions: SessionSummary[];
  interactiveTasks?: ManagedTask[];
  shellStates: ReadonlyMap<string, ShellStateSummary>;
  selectedSessionKey: string | null;
  openTabSessionKeys: ReadonlySet<string>;
  loading: boolean;
  creating: boolean;
  closingSessionKeys: ReadonlySet<string>;
  disconnectingSessionKey: string | null;
  onRefresh(): void;
  on_archives?(): void;
  onSelect(session: SessionSummary): void;
  onNewShell(): void;
  onDisconnect(session: SessionSummary): void;
  onRequestClose(session: SessionSummary): void;
  onAddHost(): void;
  onChooseHost?(): void;
  onHostSettings?(target: ConnectionTarget): void;
  onConnectHost(target: ConnectionTarget): void;
  onDisconnectHost?(target: ConnectionTarget): void;
  onRemoveHost(target: ConnectionTarget): void;
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
  connectableHostKeys,
  targetErrors,
  hostConnections,
  attachmentStates,
  sessions,
  interactiveTasks = [],
  shellStates,
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
  onRemoveHost,
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
    <aside className="session-sidebar" aria-label="rmux sessions">
      <div className="sidebar-connections">
        <header className="sidebar-header">
          <strong>Sessions</strong>
          {on_archives && <button type="button" onClick={on_archives}>Archived</button>}
          <div className="sidebar-header-actions">
            {onChooseHost ? (
              <button
                className="icon-button"
                type="button"
                onClick={onChooseHost}
                aria-label="Choose host to connect"
                title="Connect host"
              >
                <Icon name="plug" />
              </button>
            ) : null}
            <button
              className="icon-button"
              type="button"
              onClick={onAddHost}
              aria-label="Add host"
              title="Add host"
            >
              <Icon name="plus" />
            </button>
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
          const expanded = !collapsedHosts.has(key);
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
          const remoteAttachments = [...(attachmentStates?.values() ?? [])].filter((state) =>
            state.session && targetKey(state.session.target) === key);
          const timedOutAttachment = remoteAttachments.find((state) => state.error_code === "remote_connection_timeout");
          // A local mux process can outlive a stalled transport. Preserve that
          // evidence for disconnect controls without claiming remote health.
          const remoteTimeout = targetFailure?.code === "remote_connection_timeout" || !!timedOutAttachment;
          const sessionIssue = remoteTimeout ? "Remote connection timed out"
            : remoteAttachments.some((state) => ["reconnecting", "retry_wait"].includes(state.phase)) ? "Session reconnecting…"
            : remoteAttachments.some((state) => state.error_code === "automatic_reconnect_timeout") ? "Session reconnect failed" : null;
          const { state: connectionState, label: connectionLabel, route_unavailable: unavailableRoute } =
            presentation.state === "connected" && sessionIssue
              ? { ...presentation, state: "error", label: sessionIssue }
              : presentation;
          const operationLabel = operation?.state === "pending" ? operation.kind === "connect" ? "Connecting…" : "Disconnecting…"
            : operation?.state === "failed" ? operation.kind === "connect" ? "Connect failed" : "Disconnect failed" : null;
          const connectionTitle = [
            connectionLabel,
            hostError,
            presentation.state === "connected" ? "The local SSH control connection is open. This check does not freshly verify remote responsiveness or terminal health." : null,
            timedOutAttachment?.message,
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
          return (
            <section
              className="session-group host-group"
              key={key}
              aria-label={`${targetLabel(target)} sessions`}
            >
              <div className={`host-group-header ${hostError || unavailableRoute ? "has-error" : ""}`}>
                <button
                  className="host-group-toggle"
                  type="button"
                  aria-expanded={expanded}
                  aria-controls={childrenId}
                  aria-label={`${expanded ? "Collapse" : "Expand"} ${targetLabel(target)}`}
                  title={hostTitle(target)}
                  onClick={() => toggleHost(key)}
                >
                  <Icon
                    name={expanded ? "chevron_down" : "chevron_right"}
                    size={14}
                  />
                  <Icon
                    name={target.kind === "local" ? "monitor" : "server"}
                    size={15}
                  />
                  <span className="host-group-name">
                    {target.kind === "local" ? "Local" : targetLabel(target)}
                  </span>
                  {host?.source === "ssh_config" ? <span className="host-config-source" title="From SSH config; saved only when customized">SSH</span> : null}
                  {host?.source === "tailscale" ? <span className="host-config-source" title="Discovered from Tailscale; saved only when customized">Tailscale</span> : null}
                  <span className="host-group-count">{groupSessions.length}</span>
                </button>
                {target.kind === "ssh" ? (
                  <span
                    className="host-connection-status"
                    role="status"
                    aria-label={`Host connection for ${targetLabel(target)}: ${connectionLabel}${operationLabel ? ` · ${operationLabel}` : ""}`}
                    data-state={connectionState}
                    data-availability={observation?.availability}
                    data-reachability={reachability?.state}
                    data-completeness={observation?.completeness}
                    title={connectionTitle}
                  >
                    <span className="host-connection-dot" aria-hidden="true" />
                    <span>{connectionLabel}</span>
                    {operationLabel ? <span title={[operation?.method_name, operation?.message].filter(Boolean).join(": ")}> · {operationLabel}</span> : null}
                  </span>
                ) : null}
                {target.kind === "ssh" ? (
                  <div className="host-group-actions">
                    {onHostSettings ? (
                      <button
                        className="session-action"
                        type="button"
                        onClick={() => onHostSettings(target)}
                        disabled={host?.source === "unavailable"}
                        aria-label={`Host settings for ${targetLabel(target)}`}
                        title={`Host settings for ${targetLabel(target)}`}
                      >
                        <Icon name="settings" size={14} />
                      </button>
                    ) : null}
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
                    {host?.source !== "ssh_config" && host?.source !== "tailscale" ? <button
                      className="session-action"
                      type="button"
                      onClick={() => onRemoveHost(target)}
                      aria-label={`Remove ${targetLabel(target)}`}
                      title={`Remove ${targetLabel(target)} from workspace`}
                    >
                      <Icon name="close" size={14} />
                    </button> : null}
                  </div>
                ) : null}
              </div>
              <div className="host-group-children" id={childrenId} hidden={!expanded}>
                {!loading && groupSessions.length === 0 ? (
                  <p className="host-empty-state">
                    {hostError || unavailableRoute ? "Sessions unavailable" : "No known sessions"}
                  </p>
                ) : null}
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
                  const status = attachment ? attachmentPhaseLabel(attachment.phase)
                    : session.status === "running" ? "Last seen running" : observed_status;
                  const age = attachment?.phase === "attached" ? null : lastSeenAge(session.last_seen_at_ms, now_ms);
                  const observed_at = age !== null && isObservationTime(session.last_seen_at_ms)
                    ? new Date(session.last_seen_at_ms).toISOString() : null;
                  const observation_title = observed_at ? `Last observed by this app: ${observed_at}` : undefined;
                  const dimensions = attachment?.phase === "attached" || hasKnownTerminalSize(session)
                    ? ` · ${session.terminal_size.columns}×${session.terminal_size.rows}`
                    : "";
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
                        aria-description={observation_title}
                        title={fullTitle}
                      >
                        <Icon name="terminal" class_name="session-icon" />
                        <span className="session-copy">
                          <strong>{compactTitle}</strong>
                          <small className="session-details" title={[`${session.name} · ${status}${dimensions} · Session last reported ${observed_status}`, observation_title].filter(Boolean).join("\n")}>
                            <span className="session-detail-label">
                              {session.name}
                              <span aria-hidden="true"> · </span>
                              <span className="session-status" data-status={attachment?.phase ?? (session.status === "running" ? "unknown" : session.status)}>
                                {status}
                              </span>
                            </span>
                            {age !== null ? <span className="session-age">
                              <span aria-hidden="true"> · </span>
                              <time dateTime={observed_at ?? undefined} title={observation_title}>{age}</time>
                            </span> : null}
                          </small>
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
              </div>
            </section>
          );
        })}
      </div>

      <footer className="sidebar-footer">
        <button
          className="new-session-button"
          type="button"
          onClick={onAddExisting}
        >
          <Icon name="plug" size={15} /> Add existing session
        </button>
        <button
          className="new-session-button"
          type="button"
          onClick={onNewShell}
          disabled={creating}
        >
          <Icon name="plus" size={15} /> New shell
        </button>
      </footer>
    </aside>
  );
}
