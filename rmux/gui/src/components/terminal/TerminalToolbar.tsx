import type { AttachmentViewState } from "../../lib/types";

interface TerminalToolbarProps {
  state: AttachmentViewState;
  onToggleInput(): void;
  onToggleResizeWithWindow(): void;
  onReconnect(): void;
  onDetach(): void;
}

export function TerminalToolbar({
  state,
  onToggleInput,
  onToggleResizeWithWindow,
  onReconnect,
  onDetach,
}: TerminalToolbarProps) {
  const attached = state.phase === "attached";
  const canReconnect =
    state.session !== null &&
    (state.phase === "disconnected" || state.phase === "error");
  const resizeActive =
    state.resize_with_window && state.layout_lease.owned_by_client;
  const resizePending = state.resize_with_window && !resizeActive;

  return (
    <header className="terminal-toolbar">
      <div className="toolbar-session">
        <span className={`connection-dot ${state.phase}`} aria-hidden="true" />
        <div>
          <strong>{state.session?.name ?? "No session"}</strong>
          <small>{state.phase.replace("_", " ")}</small>
        </div>
      </div>
      <div className="toolbar-actions">
        {canReconnect ? (
          <button type="button" onClick={onReconnect}>Reconnect</button>
        ) : null}
        <button
          type="button"
          onClick={onToggleInput}
          disabled={!attached}
          className={state.input_lease.owned_by_client ? "active-control" : ""}
          aria-pressed={state.input_lease.owned_by_client}
          title="Input ownership is independent from terminal layout."
        >
          {state.input_lease.owned_by_client ? "Release input" : "Request input"}
        </button>
        <button
          type="button"
          onClick={onToggleResizeWithWindow}
          disabled={!attached}
          className={resizeActive ? "active-control" : ""}
          aria-pressed={resizeActive}
          title="Acquire layout ownership and keep the PTY matched to this window."
        >
          {resizePending
            ? "Starting resize…"
            : resizeActive
              ? "Stop resizing"
              : "Resize with window"}
        </button>
        <button type="button" onClick={onDetach} disabled={!state.attachment_id}>
          Detach
        </button>
      </div>
    </header>
  );
}
