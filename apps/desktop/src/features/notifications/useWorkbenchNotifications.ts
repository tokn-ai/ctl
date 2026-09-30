import { useEffect, useRef } from "react";
import type { ConnectionTarget, ManagedTask, NotificationAction } from "../../lib/types";
import { targetKey, targetLabel } from "../targets/targets";
import { COMMAND_IDS } from "../commands/commandIds";
import type { NotificationStore } from "./NotificationStore";

interface Sources {
  workspace_error: string | null;
  workspace_ready: boolean;
  keybindings_error: string | null;
  session_error: string | null;
  targets: readonly ConnectionTarget[];
  target_errors: ReadonlyMap<string, string>;
  storage_error: string | null;
  task_error: string | null;
  definitions_error: string | null;
  task_status: string | null;
  tasks: readonly ManagedTask[];
  tasks_loaded: boolean;
}

export function useWorkbenchNotifications(store: NotificationStore, sources: Sources) {
  const previous_hosts = useRef(new Set<string>());
  const previous_runs = useRef<Map<string, string> | null>(null);
  const previous_task_status = useRef<string | null>(null);
  useEffect(() => {
    const reportError = (key: string, title: string, message: string | null, actions: NotificationAction[] = []) => {
      store.report(key, message ? { severity: "error", title, message, source: title, actions } : null);
    };
    reportError("workspace", "Workspace", sources.workspace_error, sources.workspace_ready
      ? [{ label: "Retry saving", command_id: COMMAND_IDS.saveWorkspace }] : []);
    reportError("keybindings", "Keyboard shortcuts", sources.keybindings_error
      ? `Keyboard shortcuts: ${sources.keybindings_error} Last valid bindings remain active.` : null,
    [{ label: "Reload shortcuts", command_id: COMMAND_IDS.reloadKeybindings }]);
    reportError("sessions", "Sessions", sources.session_error);
    reportError("session_storage", "Session history", sources.storage_error);
    const task_status_changed = sources.task_status !== previous_task_status.current;
    previous_task_status.current = sources.task_status;
    store.report("tasks", sources.task_error ? {
      severity: "error", title: "Tasks", message: sources.task_error, source: "Tasks",
      actions: [{ label: "Retry tasks", command_id: COMMAND_IDS.refreshTasks }],
    } : task_status_changed && sources.task_status ? {
      severity: sources.task_status.endsWith("…") ? "info" : "success",
      title: "Task daemon", message: sources.task_status, source: "Tasks",
    } : null);
    reportError("task_definitions", "Task definitions", sources.definitions_error,
      [{ label: "Retry definitions", command_id: COMMAND_IDS.refreshTasks }]);

    const host_keys = new Set(sources.target_errors.keys());
    for (const key of previous_hosts.current) {
      if (!host_keys.has(key)) store.report(`host:${key}`, null);
    }
    previous_hosts.current = host_keys;
    for (const [key, message] of sources.target_errors) {
      const target = sources.targets.find((target) => targetKey(target) === key);
      reportError(`host:${key}`, target ? `Host · ${targetLabel(target)}` : "Host", message,
        target?.kind === "ssh" ? [{ label: "Connect host", command_id: COMMAND_IDS.connectHost, args: { target_key: key } }] : []);
    }

    if (sources.tasks_loaded) {
      const runs = new Map<string, string>();
      for (const task of sources.tasks) {
        const run = task.active_run ?? task.last_run;
        if (!run) continue;
        const signature = `${run.run_id}:${run.state}`;
        runs.set(task.task_id, signature);
        if (previous_runs.current && previous_runs.current.get(task.task_id) !== signature && run.state === "failed") {
          // A distinct run is a new event, even if it fails with the same exit code.
          store.report(`task_run:${task.task_id}`, null);
          store.report(`task_run:${task.task_id}`, {
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
