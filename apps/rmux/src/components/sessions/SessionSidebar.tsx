import {
  compactTerminalTitle,
  compactTerminalTitleParts,
  formatTerminalTitle,
} from "../../features/tabs/terminalTitle";
import type {
  ConnectionTarget,
  ManagedTask,
  SessionSummary,
  ShellStateSummary,
} from "../../lib/types";
import {
  sessionKey,
  targetKey,
  targetLabel,
} from "../../features/targets/targets";

const SIDEBAR_TERMINAL_TITLE_MAX_LENGTH = 20;

interface SessionSidebarProps {
  targets: readonly ConnectionTarget[];
  targetErrors: ReadonlyMap<string, string>;
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
  onConnectHost(target: ConnectionTarget): void;
  onRemoveHost(target: ConnectionTarget): void;
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

function groupLabel(target: ConnectionTarget): string {
  return target.kind === "local" ? "Local" : targetLabel(target);
}

export function SessionSidebar({
  targets,
  targetErrors,
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
  onConnectHost,
  onRemoveHost,
  onAddExisting,
  onForget,
  onSelectTask,
  onStopTask,
}: SessionSidebarProps) {
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
  const sessionGroups = targets
    .map((target) => ({
      target,
      sessions: ordinarySessions.filter(
        (session) => targetKey(session.target) === targetKey(target),
      ),
    }))
    .filter((group) => group.sessions.length > 0);
  return (
    <aside className="session-sidebar" aria-label="rmux sessions">
      <div className="sidebar-connections">
        <header className="sidebar-header">
          <strong>Sessions</strong>
          <button className="host-add-button" type="button" onClick={onAddHost}>
            + Host
          </button>
          <button
            className="icon-button"
            type="button"
            onClick={onRefresh}
            disabled={loading}
            aria-label="Refresh sessions"
            title="Refresh sessions"
          >
            ↻
          </button>
        </header>

        <div className="sidebar-hosts" aria-label="Configured hosts">
          {targets.map((target) => {
            const key = targetKey(target);
            return (
              <span className="host-chip" key={key} title={targetLabel(target)}>
                <button
                  className="host-connect"
                  type="button"
                  onClick={() => onConnectHost(target)}
                  disabled={target.kind === "local"}
                  title={`Connect to ${targetLabel(target)}`}
                >
                  {targetLabel(target)}
                </button>
                {target.kind === "ssh" ? (
                  <button
                    type="button"
                    onClick={() => onRemoveHost(target)}
                    aria-label={`Remove ${targetLabel(target)}`}
                    title={`Remove ${targetLabel(target)}`}
                  >
                    ×
                  </button>
                ) : null}
              </span>
            );
          })}
        </div>
      </div>

      <div className="session-list">
        {loading && sessions.length === 0 ? (
          <p className="sidebar-state">Loading workspace…</p>
        ) : null}
        {error ? (
          <div className="sidebar-state error-state">
            <p>{error}</p>
          </div>
        ) : null}
        {[...targetErrors.entries()].map(([key, message]) => (
          <div className="host-error" key={key} role="status">
            <strong>
              {targets.find((target) => targetKey(target) === key)
                ? targetLabel(
                    targets.find((target) => targetKey(target) === key)!,
                  )
                : "Host"}
            </strong>
            <span>{message}</span>
          </div>
        ))}
        {!loading &&
        !error &&
        targetErrors.size === 0 &&
        ordinarySessions.length === 0 && taskSessions.length === 0 ? (
          <div className="sidebar-state">
            <span className="empty-glyph">›_</span>
            <p>No known sessions.</p>
            <small>
              Create a shell or use “Add existing session” to remember one
              already running.
            </small>
          </div>
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
                  >
                    <span className="session-indicator" aria-hidden="true" />
                    <span className="session-copy">
                      <strong>{task.definition.name}</strong>
                      <small>local · {run.state}</small>
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
                      <span aria-hidden="true">■</span>
                    </button>
                  </div>
                </div>
              );
            })}
          </section>
        ) : null}
        {sessionGroups.map(({ target, sessions: groupSessions }) => (
          <section
            className="session-group"
            key={targetKey(target)}
            aria-label={`${targetLabel(target)} sessions`}
          >
            <h3>
              {groupLabel(target)} <span>{groupSessions.length}</span>
            </h3>
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
                    <span className="session-indicator" aria-hidden="true" />
                    <span className="session-copy">
                      <strong>{compactTitle}</strong>
                      <small>
                        {targetLabel(session.target)}
                        <span aria-hidden="true"> · </span>
                        {session.name}
                        <span aria-hidden="true"> · </span>
                        {session.status === "running" ? (
                          <>
                            {session.terminal_size.columns}×
                            {session.terminal_size.rows}
                            <span aria-hidden="true"> · </span>
                          </>
                        ) : null}
                        {session.status === "unknown"
                          ? "unverified"
                          : session.status}
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
                      −
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
                        <span aria-hidden="true">{disconnecting ? "…" : "⏏"}</span>
                      </button>
                    ) : null}
                    <button
                      className="session-action session-close"
                      type="button"
                      onClick={() => onRequestClose(session)}
                      disabled={closing || disconnecting}
                      aria-label={`Close ${session.name}`}
                      title="Close the session and terminate its shell"
                    >
                      <span aria-hidden="true">{closing ? "…" : "×"}</span>
                    </button>
                  </div>
                </div>
              );
            })}
          </section>
        ))}
      </div>

      <footer className="sidebar-footer">
        <button
          className="new-session-button"
          type="button"
          onClick={onAddExisting}
        >
          Add existing session
        </button>
        <button
          className="new-session-button"
          type="button"
          onClick={onNewShell}
          disabled={creating}
        >
          <span aria-hidden="true">＋</span> New shell
        </button>
      </footer>
    </aside>
  );
}
