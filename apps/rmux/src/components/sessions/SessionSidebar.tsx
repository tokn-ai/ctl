import { useId, useState } from "react";
import { Icon } from "../ui/Icon";
import {
  compactTerminalTitle,
  compactTerminalTitleParts,
  formatTerminalTitle,
} from "../../features/tabs/terminalTitle";
import type {
  ConnectionTarget,
  HostConnectionStatus,
  ManagedTask,
  SessionSummary,
  ShellStateSummary,
  WorkspaceHost,
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
  targetErrors: ReadonlyMap<string, string>;
  hostConnections?: ReadonlyMap<string, HostConnectionStatus>;
  sessions: SessionSummary[];
  interactiveTasks?: ManagedTask[];
  shellStates: ReadonlyMap<string, ShellStateSummary>;
  selectedSessionKey: string | null;
  openTabSessionKeys: ReadonlySet<string>;
  loading: boolean;
  error: string | null;
  creating: boolean;
  closingSessionKeys: ReadonlySet<string>;
  disconnectingSessionKey: string | null;
  onRefresh(): void;
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

export function SessionSidebar({
  targets,
  hosts = [],
  targetErrors,
  hostConnections,
  sessions,
  interactiveTasks = [],
  shellStates,
  selectedSessionKey,
  openTabSessionKeys,
  loading,
  error,
  creating,
  closingSessionKeys,
  disconnectingSessionKey,
  onRefresh,
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
  const sessionGroups = targets.map((target) => ({
    target,
    sessions: ordinarySessions.filter(
      (session) => targetKey(session.target) === targetKey(target),
    ),
  }));
  const orphanedErrors = [...targetErrors.entries()].filter(
    ([key]) => !targets.some((target) => targetKey(target) === key),
  );

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
        {error ? (
          <div className="sidebar-state error-state" role="status">
            <p>{error}</p>
          </div>
        ) : null}
        {orphanedErrors.map(([key, message]) => (
          <div className="host-error" key={key} role="status">
            <strong>Host</strong>
            <span>{message}</span>
          </div>
        ))}
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
          const hostError = targetErrors.get(key) ?? (target.kind === "ssh" ? target.unavailable : undefined);
          const connection = target.kind === "ssh"
            ? hostConnections?.get(target.host_id ?? "")
            : undefined;
          const unavailable = target.kind === "ssh" ? target.unavailable : undefined;
          const connectionState = unavailable ? "error" : connection?.state ?? "checking";
          const connectionLabel = unavailable ? "Unavailable" : {
            checking: "Checking…",
            connected: "Connected",
            connecting: "Connecting…",
            disconnecting: "Disconnecting…",
            disconnected: "Disconnected",
            error: "Connection error",
          }[connectionState];
          const connectionTitle = [
            connectionLabel,
            connection?.method_names.length
              ? `Connection methods: ${connection.method_names.join(", ")}`
              : null,
            unavailable ?? connection?.message,
          ].filter(Boolean).join("\n");
          const connectionBusy = connection?.state === "connecting" ||
            connection?.state === "disconnecting";
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
              <div className={`host-group-header ${hostError ? "has-error" : ""}`}>
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
                  <span className="host-group-count">{groupSessions.length}</span>
                </button>
                {target.kind === "ssh" ? (
                  <span
                    className="host-connection-status"
                    role="status"
                    aria-label={`Host connection for ${targetLabel(target)}: ${connectionLabel}`}
                    data-state={connectionState}
                    title={connectionTitle}
                  >
                    <span className="host-connection-dot" aria-hidden="true" />
                    <span>{connectionLabel}</span>
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
                        disabled={Boolean(target.unavailable) || connectionBusy}
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
                    {host?.source !== "ssh_config" ? <button
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
                {hostError ? (
                  <div className="host-error" role="status">
                    <span>{hostError}</span>
                  </div>
                ) : null}
                {!loading && groupSessions.length === 0 ? (
                  <p className="host-empty-state">
                    {hostError ? "Sessions unavailable" : "No known sessions"}
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
                  const status = session.status === "unknown"
                    ? "unverified"
                    : session.status;
                  const dimensions = session.status === "running"
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
                        title={fullTitle}
                      >
                        <Icon name="terminal" class_name="session-icon" />
                        <span className="session-copy">
                          <strong>{compactTitle}</strong>
                          <small title={`${session.name} · ${status}${dimensions}`}>
                            {session.name}
                            <span aria-hidden="true"> · </span>
                            <span className="session-status" data-status={session.status}>
                              {status}
                            </span>
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
