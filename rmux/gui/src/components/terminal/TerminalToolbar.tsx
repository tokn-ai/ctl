import type { AttachmentViewState } from "../../lib/types";

interface TerminalToolbarProps {
  state: AttachmentViewState;
  onToggleInput(): void;
  onUseWindowForLayout(): void;
  onReleaseLayout(): void;
  onReconnect(): void;
  onDetach(): void;
}

export function TerminalToolbar({
  state,
  onToggleInput,
  onUseWindowForLayout,
  onReleaseLayout,
  onReconnect,
  onDetach,
}: TerminalToolbarProps) {
  const attached = state.phase === "attached";
  const canReconnect =
    state.session !== null &&
    (state.phase === "disconnected" || state.phase === "error");

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
          title="Input ownership is independent from terminal layout."
        >
          {state.input_lease.owned_by_client ? "Release input" : "Request input"}
        </button>
        <button
          type="button"
          onClick={onUseWindowForLayout}
          disabled={!attached}
          className={state.layout_lease.owned_by_client ? "active-control" : ""}
          title="Explicitly acquire layout and resize the PTY to this window once."
        >
          Use window for layout
        </button>
        {state.layout_lease.owned_by_client ? (
          <button type="button" onClick={onReleaseLayout}>Release layout</button>
        ) : null}
        <button type="button" onClick={onDetach} disabled={!state.attachment_id}>
          Detach
        </button>
      </div>
    </header>
  );
}
