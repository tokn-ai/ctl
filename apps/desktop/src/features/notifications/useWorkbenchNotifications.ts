import { useEffect, useRef } from "react";
import type { ManagedTask, NotificationAction } from "../../lib/types";
import { COMMAND_IDS } from "../commands/commandIds";
import type { NotificationStore } from "./NotificationStore";

interface Sources {
  workspace_error: string | null;
  keybindings_error: string | null;
  storage_error: string | null;
  window_error: string | null;
  tasks: readonly ManagedTask[];
  tasks_loaded: boolean;
}

/** Only app-owned conditions and runtime events belong here. Forms own their feedback. */
export function useWorkbenchNotifications(store: NotificationStore, sources: Sources) {
  const previous_runs = useRef<Map<string, string> | null>(null);
  useEffect(() => {
    const reportError = (key: string, title: string, message: string | null, actions: NotificationAction[] = [], toast = true) => {
      if (!message) { store.resolve(key); return; }
      store.report(key, { severity: "error", title, message, source: title, actions }, { toast });
    };
    reportError("workspace", "Workspace", sources.workspace_error,
      [{ label: "Retry saving", command_id: COMMAND_IDS.saveWorkspace }], false);
    reportError("keybindings", "Keyboard shortcuts", sources.keybindings_error
      ? `Keyboard shortcuts: ${sources.keybindings_error} Last valid bindings remain active.` : null,
      [{ label: "Reload shortcuts", command_id: COMMAND_IDS.reloadKeybindings }]);
    reportError("session_storage", "Session history", sources.storage_error);
    reportError("window_close", "Window", sources.window_error);

    if (sources.tasks_loaded) {
      const runs = new Map<string, string>();
      for (const task of sources.tasks) {
        const run = task.active_run ?? task.last_run;
        if (!run) continue;
        const signature = `${run.run_id}:${run.state}`;
        runs.set(task.task_id, signature);
        if (previous_runs.current && previous_runs.current.get(task.task_id) !== signature && run.state === "failed") {
          store.notify(`task_run:${task.task_id}`, {
            severity: "error", title: "Task failed", source: task.definition.name,
            message: `${task.definition.name} failed${run.exit_code !== null ? ` with exit code ${run.exit_code}` : ""}.`,
            actions: [{ label: "View task", command_id: COMMAND_IDS.openTask, args: { value: task.task_id } }],
          });
        }
      }
      previous_runs.current = runs;
    }
  });
}
