import type { AttachmentViewState } from "../../lib/types";

function readable(value: string): string {
  return value.replace(/_/g, " ");
}

export interface StatusItem {
  key: string;
  label: string;
  className?: string;
  title?: string;
}

export function createStatusItems(state: AttachmentViewState): StatusItem[] {
  const items: StatusItem[] = [];
  const shell = state.shell_state;
  const size = state.session?.terminal_size;

  if (
    state.phase === "attached" &&
    !state.input_lease.owned_by_client
  ) {
    items.push({ key: "input", label: "view only", className: "status-warning" });
  }
  if (size) {
    items.push({ key: "size", label: `${size.columns}×${size.rows}` });
  }
  if (shell && shell.shell_type !== "unknown") {
    items.push({ key: "shell", label: shell.shell_type });
  }
  if (shell && shell.prompt_phase !== "unknown") {
    items.push({ key: "prompt", label: readable(shell.prompt_phase) });
  }
  if (shell?.tui_hint === "alternate_screen") {
    items.push({ key: "tui", label: "TUI" });
  }
  if (shell?.cwd) {
    items.push({
      key: "cwd",
      label: shell.cwd,
      className: "status-cwd",
      title: shell.cwd,
    });
  }

  return items;
}
