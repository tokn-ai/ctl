import type { FormEvent } from "react";
import { useState } from "react";
import type { SessionSummary } from "../../lib/types";

interface SessionSidebarProps {
  sessions: SessionSummary[];
  activeSessionId: string | null;
  loading: boolean;
  error: string | null;
  creating: boolean;
  onRefresh(): void;
  onSelect(session: SessionSummary): void;
  onCreate(name: string | null, workingDirectory: string | null): void;
}

export function SessionSidebar({
  sessions,
  activeSessionId,
  loading,
  error,
  creating,
  onRefresh,
  onSelect,
  onCreate,
}: SessionSidebarProps) {
  const [showCreate, setShowCreate] = useState(false);
  const [name, setName] = useState("");
  const [workingDirectory, setWorkingDirectory] = useState("");

  function submit(event: FormEvent) {
    event.preventDefault();
    onCreate(name.trim() || null, workingDirectory.trim() || null);
  }

  return (
    <aside className="session-sidebar" aria-label="rmux sessions">
      <header className="sidebar-header">
        <div>
          <span className="eyebrow">LOCAL RMUX</span>
          <h1>Sessions</h1>
        </div>
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
        {sessions.map((session) => (
          <button
            className={`session-row ${
              session.session_id === activeSessionId ? "active" : ""
            }`}
            type="button"
            key={session.session_id}
            onClick={() => onSelect(session)}
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
        ))}
      </div>

      <footer className="sidebar-footer">
        {showCreate ? (
          <form className="new-session-form" onSubmit={submit}>
            <label>
              Session name
              <input
                value={name}
                onChange={(event) => setName(event.currentTarget.value)}
                placeholder="optional"
                autoFocus
              />
            </label>
            <label>
              Working directory
              <input
                value={workingDirectory}
                onChange={(event) => setWorkingDirectory(event.currentTarget.value)}
                placeholder="home directory"
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
                {creating ? "Creating…" : "Create"}
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
