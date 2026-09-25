import type {
  AttachmentViewState,
  ConnectionPhase,
  PromptPhase,
} from "../../lib/types";
import { displayWorkingDirectory } from "../../lib/shellState";

export type StatusItemPriority = "high" | "medium" | "low";
export type StatusItemTone = "normal" | "warning" | "danger";

export interface StatusItem {
  key: string;
  label: string;
  title: string;
  priority: StatusItemPriority;
  tone: StatusItemTone;
  flexible?: boolean;
}

export interface StatusGroups {
  context: StatusItem[];
  indicators: StatusItem[];
}

function item(
  key: string,
  label: string,
  title: string,
  options: Partial<Pick<StatusItem, "priority" | "tone" | "flexible">> = {},
): StatusItem {
  return {
    key,
    label,
    title,
    priority: options.priority ?? "high",
    tone: options.tone ?? "normal",
    flexible: options.flexible,
  };
}

function connectionStatus(phase: ConnectionPhase): StatusItem | null {
  switch (phase) {
    case "connecting":
      return item("connection", "CONNECTING", "Connecting to the rmux session.", {
        tone: "warning",
      });
    case "reconnecting":
      return item("connection", "RECONNECTING", "Restoring the rmux attachment.", {
        tone: "warning",
      });
    case "disconnected":
      return item("connection", "DISCONNECTED", "The rmux attachment is disconnected.", {
        tone: "warning",
      });
    case "ended":
      return item("connection", "ENDED", "The shell session has ended.", {
        tone: "warning",
      });
    case "error":
      return item("connection", "ERROR", "The rmux attachment encountered an error.", {
        tone: "danger",
      });
    case "idle":
    case "attached":
      return null;
  }
}

function promptStatus(promptPhase: PromptPhase): StatusItem | null {
  switch (promptPhase) {
    case "at_prompt":
      return item("activity", "READY", "The shell is ready at an empty prompt.", {
        priority: "medium",
      });
    case "editing":
      return item("activity", "EDITING", "The shell is editing a command line.", {
        priority: "medium",
      });
    case "running":
      return item("activity", "RUNNING", "The shell is waiting for a command to finish.", {
        priority: "medium",
      });
    case "unknown":
      return null;
  }
}

function inputStatus(state: AttachmentViewState): StatusItem | null {
  if (state.phase !== "attached") {
    return null;
  }
  if (state.input_lease.owned_by_client) {
    return item("input", "INPUT", "This window owns terminal input.");
  }

  const title = state.input_lease.held
    ? "View only: another attachment owns terminal input."
    : "View only: this window does not own terminal input.";
  return item("input", "VIEW", title, { tone: "warning" });
}

function layoutStatus(state: AttachmentViewState): StatusItem | null {
  if (state.phase !== "attached") {
    return null;
  }
  if (state.resize_with_window && state.layout_lease.owned_by_client) {
    return item(
      "layout",
      "WINDOW",
      "This window owns layout and resizes the PTY with its terminal pane.",
    );
  }
  if (state.layout_lease.held && !state.layout_lease.owned_by_client) {
    return item(
      "layout",
      "OTHER SIZE",
      "Another attachment owns the PTY layout.",
      { priority: "medium" },
    );
  }
  if (state.resize_with_window) {
    return item(
      "layout",
      "STARTING SIZE",
      "Waiting to acquire layout ownership for window resizing.",
      { priority: "medium", tone: "warning" },
    );
  }
  return item(
    "layout",
    "FIXED",
    "The PTY keeps its current size until a client takes layout ownership.",
    { priority: "medium" },
  );
}

export function createStatusGroups(state: AttachmentViewState): StatusGroups {
  const context: StatusItem[] = [];
  const indicators: StatusItem[] = [];
  const alerts: StatusItem[] = [];
  const shell = state.shell_state;
  const size = state.session?.terminal_size;

  if (shell && shell.shell_type !== "unknown") {
    context.push(item("shell", shell.shell_type, `Shell: ${shell.shell_type}`, {
      priority: "low",
    }));
  }
  const cwdDisplay = shell ? displayWorkingDirectory(shell) : null;
  if (cwdDisplay) {
    context.push(item("cwd", cwdDisplay, cwdDisplay, { flexible: true }));
  }

  const connection = connectionStatus(state.phase);
  if (connection) {
    alerts.push(connection);
  }
  if (state.history_gap) {
    alerts.push(item(
      "history",
      "HISTORY GAP",
      "Earlier output is no longer contiguous; the live screen was restored from a checkpoint.",
      { tone: "warning" },
    ));
  }

  if (shell) {
    const activity = promptStatus(shell.prompt_phase);
    if (activity) {
      indicators.push(activity);
    }
    if (shell.tui_hint === "alternate_screen") {
      indicators.push(item(
        "tui",
        "TUI",
        "The terminal parser observed the alternate screen buffer.",
        { priority: "medium" },
      ));
    }
  }

  const input = inputStatus(state);
  if (input) {
    indicators.push(input);
  }
  const layout = layoutStatus(state);
  if (layout) {
    indicators.push(layout);
  }
  if (size) {
    indicators.push(item(
      "size",
      `${size.columns}×${size.rows}`,
      `PTY size: ${size.columns} columns by ${size.rows} rows.`,
    ));
  }
  indicators.push(...alerts);

  return { context, indicators };
}
