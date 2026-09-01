import type { FormEvent } from "react";
import { useEffect, useState } from "react";
import {
  compactTerminalTitle,
  compactTerminalTitleParts,
  formatTerminalTitle,
} from "../../features/tabs/terminalTitle";
import type {
  ConnectionTarget,
  SessionSummary,
  ShellStateSummary,
} from "../../lib/types";
import { sessionKey, targetKey, targetLabel } from "../../features/targets/targets";
import { SshHostPicker } from "./SshHostPicker";

const SIDEBAR_TERMINAL_TITLE_MAX_LENGTH = 20;

interface SessionSidebarProps {
  targets: readonly ConnectionTarget[];
  targetErrors: ReadonlyMap<string, string>;
  sessions: SessionSummary[];
  shellStates: ReadonlyMap<string, ShellStateSummary>;
  selectedSessionKey: string | null;
  openTabSessionKeys: ReadonlySet<string>;
  loading: boolean;
  error: string | null;
  creating: boolean;
  createFormOpen: boolean;
  pendingCloseSessionKey: string | null;
  closingSessionKeys: ReadonlySet<string>;
  disconnectingSessionKey: string | null;
  hostSuggestions: readonly string[];
  hostSuggestionWarning: string | null;
  onRefresh(): void;
  onSelect(session: SessionSummary): void;
  onCreate(target: ConnectionTarget, workingDirectory: string | null): Promise<boolean>;
  onCreateFormOpenChange(open: boolean): void;
  onDisconnect(session: SessionSummary): void;
  onRequestClose(session: SessionSummary): void;
  onCancelClose(): void;
  onConfirmClose(session: SessionSummary): void;
  onAddHost(destination: string): boolean;
  onRemoveHost(target: ConnectionTarget): void;
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

export function SessionSidebar({
  targets,
  targetErrors,
  sessions,
  shellStates,
  selectedSessionKey,
  openTabSessionKeys,
  loading,
  error,
  creating,
  createFormOpen,
  pendingCloseSessionKey,
  closingSessionKeys,
  disconnectingSessionKey,
  hostSuggestions,
  hostSuggestionWarning,
  onRefresh,
  onSelect,
  onCreate,
  onCreateFormOpenChange,
  onDisconnect,
  onRequestClose,
  onCancelClose,
  onConfirmClose,
  onAddHost,
  onRemoveHost,
}: SessionSidebarProps) {
  const [workingDirectory, setWorkingDirectory] = useState("");
  const [creationTargetKey, setCreationTargetKey] = useState("local");
  const [hostFormOpen, setHostFormOpen] = useState(false);

  useEffect(() => {
    if (!targets.some((target) => targetKey(target) === creationTargetKey)) {
      setCreationTargetKey("local");
    }
  }, [creationTargetKey, targets]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const target =
      targets.find((candidate) => targetKey(candidate) === creationTargetKey) ??
      targets[0];
    if (!target) {
      return;
    }
    const created = await onCreate(target, workingDirectory.trim() || null);
    if (created) {
      setWorkingDirectory("");
      onCreateFormOpenChange(false);
    }
  }

  return (
    <aside className="session-sidebar" aria-label="rmux sessions">
      <div className="sidebar-connections">
        <header className="sidebar-header">
          <strong>Sessions</strong>
          <button
            className="host-add-button"
            type="button"
            onClick={() => setHostFormOpen((open) => !open)}
            aria-expanded={hostFormOpen}
          >
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
                <span>{targetLabel(target)}</span>
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

        {hostFormOpen ? (
          <SshHostPicker
            suggestions={hostSuggestions}
            warning={hostSuggestionWarning}
            onAddHost={onAddHost}
            onClose={() => setHostFormOpen(false)}
          />
        ) : null}
      </div>

      <div className="session-list">
        {loading && sessions.length === 0 ? (
          <p className="sidebar-state">Finding sessions across hosts…</p>
        ) : null}
        {error ? (
          <div className="sidebar-state error-state"><p>{error}</p></div>
        ) : null}
        {[...targetErrors.entries()].map(([key, message]) => (
          <div className="host-error" key={key} role="status">
            <strong>{key === "local" ? "local" : key.slice(4)}</strong>
            <span>{message}</span>
          </div>
        ))}
        {!loading && !error && targetErrors.size === 0 && sessions.length === 0 ? (
          <div className="sidebar-state">
            <span className="empty-glyph">›_</span>
            <p>No running sessions.</p>
            <small>Create one here or with the rmux CLI.</small>
          </div>
        ) : null}
        {sessions.map((session) => {
          const identity = sessionKey(session);
          const { fullTitle, compactTitle } = sidebarTitle(
            session,
            shellStates.get(identity) ?? null,
          );
          const selected = identity === selectedSessionKey;
          const closing = closingSessionKeys.has(identity);
          const disconnecting = identity === disconnectingSessionKey;
          const canDisconnect = openTabSessionKeys.has(identity);
          const confirmingClose = identity === pendingCloseSessionKey;
          return (
            <div
              className={`session-row ${selected ? "active" : ""} ${
                confirmingClose ? "confirming-close" : ""
              }`}
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
                    {session.terminal_size.columns}×{session.terminal_size.rows}
                    <span aria-hidden="true"> · </span>
                    {session.status}
                  </small>
                </span>
              </button>
              {confirmingClose ? (
                <div
                  className="session-close-confirmation"
                  role="group"
                  aria-label={`Confirm closing ${session.name}`}
                  onKeyDown={(event) => {
                    if (event.nativeEvent.isComposing || event.key !== "Escape") {
                      return;
                    }
                    event.preventDefault();
                    event.stopPropagation();
                    onCancelClose();
                  }}
                >
                  <span>
                    Terminate <strong>{session.name}</strong> for all clients?
                  </span>
                  <div className="session-confirmation-actions">
                    <button
                      className="session-confirm-cancel"
                      type="button"
                      onClick={onCancelClose}
                    >
                      Cancel
                    </button>
                    <button
                      className="session-confirm-close"
                      type="button"
                      onClick={() => onConfirmClose(session)}
                      autoFocus
                    >
                      Close
                    </button>
                  </div>
                </div>
              ) : (
                <div className="session-actions">
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
              )}
            </div>
          );
        })}
      </div>

      <footer className="sidebar-footer">
        {createFormOpen ? (
          <form className="new-session-form" onSubmit={submit}>
            <label>
              Host
              <select
                value={creationTargetKey}
                onChange={(event) => setCreationTargetKey(event.currentTarget.value)}
              >
                {targets.map((target) => (
                  <option key={targetKey(target)} value={targetKey(target)}>
                    {targetLabel(target)}
                  </option>
                ))}
              </select>
            </label>
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
                onClick={() => onCreateFormOpenChange(false)}
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
            onClick={() => onCreateFormOpenChange(true)}
          >
            <span aria-hidden="true">＋</span> New shell
          </button>
        )}
      </footer>
    </aside>
  );
}
