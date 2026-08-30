import type { AttachmentViewState } from "../../lib/types";

function readable(value: string): string {
  return value.replace(/_/g, " ");
}

interface StatusBarProps {
  state: AttachmentViewState;
}

export function StatusBar({ state }: StatusBarProps) {
  const shell = state.shell_state;
  const size = state.session?.terminal_size;
  return (
    <footer className="status-bar">
      <span className={state.input_lease.owned_by_client ? "status-good" : ""}>
        {state.input_lease.owned_by_client ? "input" : "view only"}
      </span>
      <span className={state.layout_lease.owned_by_client ? "status-good" : ""}>
        {state.layout_lease.owned_by_client ? "layout owner" : "layout viewer"}
      </span>
      {size ? <span>{size.columns}×{size.rows}</span> : null}
      {shell ? <span>{shell.shell_type}</span> : null}
      {shell ? <span>{readable(shell.prompt_phase)}</span> : null}
      {shell?.tui_hint === "alternate_screen" ? <span>TUI</span> : null}
      {shell?.cwd ? (
        <span className="status-cwd" title={shell.cwd}>{shell.cwd}</span>
      ) : null}
      {state.applied_sequence ? (
        <span className="status-sequence" title="Renderer-applied raw sequence">
          seq {state.applied_sequence}
        </span>
      ) : null}
    </footer>
  );
}
