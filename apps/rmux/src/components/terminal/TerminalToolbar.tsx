import { Icon } from "../ui/Icon";
import type { AttachmentViewState } from "../../lib/types";
import { targetLabel } from "../../features/targets/targets";

interface TerminalToolbarProps {
  state: AttachmentViewState;
  showInputControl?: boolean;
  onToggleInput(): void;
  onToggleResizeWithWindow(): void;
  onReconnect(): void;
  onShowCommands(): void;
  commandShortcutLabel: string;
}

export function TerminalToolbar({
  state,
  showInputControl = true,
  onToggleInput,
  onToggleResizeWithWindow,
  onReconnect,
  onShowCommands,
  commandShortcutLabel,
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
          <small>
            {state.session ? `${targetLabel(state.session.target)} · ` : ""}
            {state.phase.replace("_", " ")}
          </small>
        </div>
      </div>
      <div className="toolbar-actions">
        {canReconnect ? (
          <button type="button" onClick={onReconnect}>
            <Icon name="refresh" size={14} /> Reconnect
          </button>
        ) : null}
        {showInputControl && <button
          type="button"
          onClick={onToggleInput}
          disabled={!attached}
          className={state.input_lease.owned_by_client ? "active-control" : ""}
          aria-pressed={state.input_lease.owned_by_client}
          aria-label={state.input_lease.owned_by_client ? "Release input" : "Request input"}
          title={state.input_lease.owned_by_client ? "Release input to make this terminal read-only" : "Request input control for this terminal"}
        >
          <Icon name="keyboard" size={14} />
          {state.input_lease.owned_by_client ? "Input enabled" : "Read only"}
        </button>}
        <button
          type="button"
          onClick={onToggleResizeWithWindow}
          disabled={!attached}
          className={resizeActive ? "active-control" : ""}
          aria-pressed={resizeActive}
          aria-label={resizePending ? "Starting resize…" : resizeActive ? "Stop resizing" : "Resize with window"}
          title="Control the shared view size from this window"
        >
          <Icon name="monitor" size={14} />
          {resizePending ? "Resizing…" : resizeActive ? "Auto resize" : "Fixed size"}
        </button>
        <button
          className="command-palette-trigger"
          type="button"
          onClick={onShowCommands}
          title={`Show command palette${commandShortcutLabel ? ` (${commandShortcutLabel})` : ""}`}
        >
          <Icon name="command" size={14} />
          <span>Commands</span>
          {commandShortcutLabel ? (
            <kbd aria-hidden="true">{commandShortcutLabel}</kbd>
          ) : null}
        </button>
      </div>
    </header>
  );
}
