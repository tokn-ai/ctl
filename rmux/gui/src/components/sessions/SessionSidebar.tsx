import type { FormEvent } from "react";
import { useState } from "react";
import type { SessionSummary } from "../../lib/types";

interface SessionSidebarProps {
  sessions: SessionSummary[];
  selectedSessionId: string | null;
  disconnectableSessionId: string | null;
  loading: boolean;
  error: string | null;
  creating: boolean;
  closingSessionIds: ReadonlySet<string>;
  disconnectingSessionId: string | null;
  onRefresh(): void;
  onSelect(session: SessionSummary): void;
  onCreate(workingDirectory: string | null): Promise<boolean>;
  onDisconnect(session: SessionSummary): void;
  onClose(session: SessionSummary): void;
}

export function SessionSidebar({
  sessions,
  selectedSessionId,
  disconnectableSessionId,
  loading,
  error,
  creating,
  closingSessionIds,
  disconnectingSessionId,
  onRefresh,
  onSelect,
  onCreate,
  onDisconnect,
  onClose,
}: SessionSidebarProps) {
  const [showCreate, setShowCreate] = useState(false);
  const [workingDirectory, setWorkingDirectory] = useState("");

  async function submit(event: FormEvent) {
    event.preventDefault();
    const created = await onCreate(workingDirectory.trim() || null);
    if (created) {
      setWorkingDirectory("");
      setShowCreate(false);
    }
  }

  function requestClose(session: SessionSummary) {
    const confirmed = window.confirm(
      `Close "${session.name}"? This terminates its shell and disconnects every attached client.`,
    );
    if (confirmed) {
      onClose(session);
    }
  }

  return (
    <aside className="session-sidebar" aria-label="rmux sessions">
      <header className="sidebar-header">
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

      <div className="session-list">
        {loading && sessions.length === 0 ? (
          <p className="sidebar-state">Finding local sessions…</p>
        ) : null}
        {error ? (
          <div className="sidebar-state error-state">
            <p>{error}</p>
            <button type="button" onClick={onRefresh}>Retry</button>
          </div>
        ) : null}
        {!loading && !error && sessions.length === 0 ? (
          <div className="sidebar-state">
            <span className="empty-glyph">›_</span>
            <p>No running sessions.</p>
            <small>Create one here or with the rmux CLI.</small>
          </div>
        ) : null}
        {sessions.map((session) => {
          const selected = session.session_id === selectedSessionId;
          const closing = closingSessionIds.has(session.session_id);
          const disconnecting = session.session_id === disconnectingSessionId;
          const canDisconnect = session.session_id === disconnectableSessionId;
          return (
            <div
              className={`session-row ${selected ? "active" : ""}`}
              key={session.session_id}
            >
              <button
                className="session-select"
                type="button"
                onClick={() => onSelect(session)}
                disabled={closing}
                aria-current={selected ? "true" : undefined}
              >
                <span className="session-indicator" aria-hidden="true" />
                <span className="session-copy">
                  <strong>{session.name}</strong>
                  <small>
                    {session.terminal_size.columns}×{session.terminal_size.rows}
                    <span aria-hidden="true"> · </span>
                    {session.status}
                  </small>
                </span>
              </button>
              <div className="session-actions">
                {canDisconnect ? (
                  <button
                    className="session-action"
                    type="button"
                    onClick={() => onDisconnect(session)}
                    disabled={disconnecting || closing}
                    aria-label={`Disconnect from ${session.name}`}
                    title="Disconnect this window; keep the session running"
                  >
                    <span aria-hidden="true">{disconnecting ? "…" : "⏏"}</span>
                  </button>
                ) : null}
                <button
                  className="session-action session-close"
                  type="button"
                  onClick={() => requestClose(session)}
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
      </div>

      <footer className="sidebar-footer">
        {showCreate ? (
          <form className="new-session-form" onSubmit={submit}>
            <label>
              Working directory
              <input
                value={workingDirectory}
                onChange={(event) => setWorkingDirectory(event.currentTarget.value)}
                placeholder="home directory"
                autoFocus
              />
            </label>
            <div className="form-actions">
              <button
                className="button-secondary"
                type="button"
                onClick={() => setShowCreate(false)}
              >
                Cancel
              </button>
              <button className="button-primary" type="submit" disabled={creating}>
                {creating ? "Creating…" : "Create shell"}
              </button>
            </div>
          </form>
        ) : (
          <button
            className="new-session-button"
            type="button"
            onClick={() => setShowCreate(true)}
          >
            <span aria-hidden="true">＋</span> New shell
          </button>
        )}
      </footer>
    </aside>
  );
}
