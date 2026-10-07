import { Icon } from "../ui/Icon";
import type { AttachmentViewState } from "../../lib/types";
import { targetLabel } from "../../features/targets/targets";
import { attachmentPhaseLabel } from "../../features/attachment/attachmentState";
import type { ResizeControlStatus } from "../../features/attachment/componentActions";

interface TerminalToolbarProps {
  state: AttachmentViewState;
  showInputControl?: boolean;
  onToggleInput(): void;
  onToggleResizeWithWindow(): void;
  resize_control_status: ResizeControlStatus;
  onRequestResizeControl(acquire: boolean): void;
  onReconnect(): void;
  onShowCommands(): void;
  commandShortcutLabel: string;
}

export function TerminalToolbar({
  state,
  showInputControl = true,
  onToggleInput,
  onToggleResizeWithWindow,
  resize_control_status,
  onRequestResizeControl,
  onReconnect,
  onShowCommands,
  commandShortcutLabel,
}: TerminalToolbarProps) {
  const attached = state.phase === "attached";
  const canReconnect =
    state.session !== null &&
    (state.phase === "disconnected" || state.phase === "error");
  const resizeOwned = resize_control_status === "owned";
  const resizeActive = state.resize_with_window && resizeOwned;
  const resizeAvailable = resize_control_status === "available";
  const resizeLabel = resizeOwned ? "Owned here" : resizeAvailable ? "Available"
    : resize_control_status === "held_elsewhere" ? "Held elsewhere" : "Unavailable";

  return (
    <header className="terminal-toolbar">
      <div className="toolbar-session">
        <span className={`connection-dot ${state.phase}`} aria-hidden="true" />
        <div>
          <strong>{state.session?.name ?? "No session"}</strong>
          <small>
            {state.session ? `${targetLabel(state.session.target)} · ` : ""}
            {attachmentPhaseLabel(state.phase)}
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
        <span aria-label="Resize control status">Resize: {resizeLabel}</span>
        <button
          type="button"
          onClick={() => onRequestResizeControl(!resizeOwned)}
          disabled={!attached || resize_control_status === "unavailable"}
          className={resizeOwned ? "active-control" : ""}
          aria-pressed={resizeOwned}
          title={resizeOwned ? "Release shared view resize control" : resizeAvailable
            ? "Take shared view resize control for pane sizing and zoom"
            : resize_control_status === "held_elsewhere" ? "Request resize control; the current owner must release it first" : "Attach to a running session before taking resize control"}
        >
          <Icon name="monitor" size={14} />
          {resizeOwned ? "Release resize control" : "Take resize control"}
        </button>
        <button
          type="button"
          onClick={onToggleResizeWithWindow}
          disabled={!attached}
          className={resizeActive ? "active-control" : ""}
          aria-pressed={state.resize_with_window}
          aria-label={state.resize_with_window ? "Use fixed size" : "Resize with window"}
          title={state.resize_with_window ? "Stop following window size and retain resize control" : "Follow this window size automatically"}
        >
          <Icon name="monitor" size={14} />
          {state.resize_with_window ? "Auto resize" : "Fixed size"}
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
