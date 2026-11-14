import { useEffect, useRef } from "react";
import type { ManagedTask, NotificationAction } from "../../lib/types";
import { COMMAND_IDS } from "../commands/commandIds";
import { message, type DisplayText } from "../i18n/messages";
import type { NotificationStore } from "./NotificationStore";

interface Sources {
  workspace_error: DisplayText | null;
  keybindings_error: DisplayText | null;
  storage_error: DisplayText | null;
  window_error: DisplayText | null;
  tasks: readonly ManagedTask[];
  tasks_loaded: boolean;
}

/** Only app-owned conditions and runtime events belong here. Forms own their feedback. */
export function useWorkbenchNotifications(store: NotificationStore, sources: Sources) {
  const previous_runs = useRef<Map<string, string> | null>(null);
  useEffect(() => {
    const reportError = (key: string, title: DisplayText, detail: DisplayText | null, actions: NotificationAction[] = [], toast = true) => {
      if (!detail) { store.resolve(key); return; }
      store.report(key, { severity: "error", title, message: detail, source: title, actions }, { toast });
    };
    reportError("workspace", message("command.category_workspace"), sources.workspace_error,
      [{ label: message("notifications.retry_save"), command_id: COMMAND_IDS.saveWorkspace }], false);
    reportError("keybindings", message("shortcuts.title"), sources.keybindings_error
      ? message("notifications.keybindings_error", { detail: sources.keybindings_error }) : null,
      [{ label: message("notifications.reload_shortcuts"), command_id: COMMAND_IDS.reloadKeybindings }]);
    reportError("session_storage", message("notifications.session_history"), sources.storage_error);
    reportError("window_close", message("notifications.window"), sources.window_error);

    if (sources.tasks_loaded) {
      const runs = new Map<string, string>();
      for (const task of sources.tasks) {
        const run = task.active_run ?? task.last_run;
        if (!run) continue;
        const signature = `${run.run_id}:${run.state}`;
        runs.set(task.task_id, signature);
        if (previous_runs.current && previous_runs.current.get(task.task_id) !== signature && run.state === "failed") {
          store.notify(`task_run:${task.task_id}`, {
            severity: "error", title: message("notifications.task_failed"), source: task.definition.name,
            message: run.exit_code === null
              ? message("notifications.task_failed_named", { name: task.definition.name })
              : message("notifications.task_failed_with_exit", { name: task.definition.name, exit_code: run.exit_code }),
            actions: [{ label: message("notifications.view_task"), command_id: COMMAND_IDS.openTask, args: { value: task.task_id } }],
          });
        }
      }
      previous_runs.current = runs;
    }
  });
}
