import { translate, type Locale, type MessageValues } from "../../features/i18n/messages";
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

type Translate = (id: string, values?: MessageValues) => string;

function connectionStatus(phase: ConnectionPhase, t: Translate): StatusItem | null {
  switch (phase) {
    case "connecting":
      return item("connection", t("status_detail.connecting"), t("status_detail.connecting_hint"), {
        tone: "warning",
      });
    case "reconnecting":
      return item("connection", t("status_detail.reconnecting"), t("status_detail.reconnecting_hint"), {
        tone: "warning",
      });
    case "retry_wait":
      return item("connection", t("status_detail.waiting"), t("status_detail.waiting_hint"), { tone: "warning" });
    case "disconnected":
      return item("connection", t("status_detail.disconnected"), t("status_detail.disconnected_hint"), {
        tone: "warning",
      });
    case "ended":
      return item("connection", t("status_detail.ended"), t("status_detail.ended_hint"), {
        tone: "warning",
      });
    case "error":
      return item("connection", t("status_detail.error"), t("status_detail.error_hint"), {
        tone: "danger",
      });
    case "idle":
    case "attached":
      return null;
  }
}

function promptStatus(promptPhase: PromptPhase, t: Translate): StatusItem | null {
  switch (promptPhase) {
    case "at_prompt":
      return item("activity", t("status_detail.ready"), t("status_detail.ready_hint"), {
        priority: "medium",
      });
    case "editing":
      return item("activity", t("status_detail.editing"), t("status_detail.editing_hint"), {
        priority: "medium",
      });
    case "running":
      return item("activity", t("status_detail.running"), t("status_detail.running_hint"), {
        priority: "medium",
      });
    case "unknown":
      return null;
  }
}

function inputStatus(state: AttachmentViewState, t: Translate): StatusItem | null {
  if (state.phase !== "attached") {
    return null;
  }
  if (state.input_lease.owned_by_client) {
    return item("input", t("status_detail.input"), t("status_detail.input_hint"));
  }

  const title = state.input_lease.held
    ? t("status_detail.view_other_hint")
    : t("status_detail.view_not_owned_hint");
  return item("input", t("status_detail.view"), title, { tone: "warning" });
}

function layoutStatus(state: AttachmentViewState, t: Translate): StatusItem | null {
  if (state.phase !== "attached") {
    return null;
  }
  if (state.resize_with_window && state.layout_lease.owned_by_client) {
    return item(
      "layout",
      t("status_detail.layout_window"),
      t("status_detail.layout_window_hint"),
    );
  }
  if (state.layout_lease.held && !state.layout_lease.owned_by_client) {
    return item(
      "layout",
      t("status_detail.layout_other"),
      t("status_detail.layout_other_hint"),
      { priority: "medium" },
    );
  }
  if (state.resize_with_window) {
    return item(
      "layout",
      t("status_detail.layout_waiting"),
      t("status_detail.layout_waiting_hint"),
      { priority: "medium", tone: "warning" },
    );
  }
  return item(
    "layout",
    t("status_detail.layout_fixed"),
    state.layout_lease.owned_by_client
      ? t("status_detail.layout_fixed_owned_hint")
      : t("status_detail.layout_fixed_hint"),
    { priority: "medium" },
  );
}

export function createStatusGroups(state: AttachmentViewState, locale: Locale = "en"): StatusGroups {
  const t: Translate = (id, values) => translate(locale, id, values);
  const context: StatusItem[] = [];
  const indicators: StatusItem[] = [];
  const alerts: StatusItem[] = [];
  const shell = state.shell_state;
  const size = state.session?.terminal_size;

  if (shell && shell.shell_type !== "unknown") {
    context.push(item("shell", shell.shell_type, t("status_detail.shell", { shell: shell.shell_type }), {
      priority: "low",
    }));
  }
  const cwdDisplay = shell ? displayWorkingDirectory(shell) : null;
  if (cwdDisplay) {
    context.push(item("cwd", cwdDisplay, cwdDisplay, { flexible: true }));
  }

  const connection = connectionStatus(state.phase, t);
  if (connection) {
    alerts.push(connection);
  }
  if (state.history_gap && !state.history_loading) {
    alerts.push(item(
      "history",
      t("status_detail.history_gap"),
      t("status_detail.history_gap_hint"),
      { tone: "warning" },
    ));
  }

  if (shell && state.phase === "attached") {
    const activity = promptStatus(shell.prompt_phase, t);
    if (activity) {
      indicators.push(activity);
    }
    if (shell.tui_hint === "alternate_screen") {
      indicators.push(item(
        "tui",
        t("status_detail.tui"),
        t("status_detail.tui_hint"),
        { priority: "medium" },
      ));
    }
  } else if (shell) {
    indicators.push(item("activity", t("status_detail.last_known"), t("status_detail.last_known_hint"), { tone: "warning", priority: "medium" }));
  }

  const input = inputStatus(state, t);
  if (input) {
    indicators.push(input);
  }
  const layout = layoutStatus(state, t);
  if (layout) {
    indicators.push(layout);
  }
  if (size) {
    indicators.push(item(
      "size",
      `${size.columns}×${size.rows}`,
      t("status_detail.pty_size", { columns: size.columns, rows: size.rows }),
    ));
  }
  indicators.push(...alerts);

  return { context, indicators };
}
