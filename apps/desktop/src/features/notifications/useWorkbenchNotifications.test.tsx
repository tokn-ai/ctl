// @vitest-environment jsdom
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ManagedTask } from "../../lib/types";
import { NotificationStore } from "./NotificationStore";
import { useWorkbenchNotifications } from "./useWorkbenchNotifications";
import { COMMAND_IDS } from "../commands/commandIds";
import { targetKey } from "../targets/targets";

afterEach(cleanup);

const sources: Parameters<typeof useWorkbenchNotifications>[1] = {
  workspace_error: null, workspace_ready: true, keybindings_error: null, session_error: null,
  targets: [], target_errors: new Map(), storage_error: null,
  task_error: null, definitions_error: null, task_status: null, tasks: [], tasks_loaded: false,
};

describe("workbench notification sources", () => {
  it("updates a progress card with failure instead of leaving a stale operation running", () => {
    const store = new NotificationStore();
    const hook = renderHook((props) => useWorkbenchNotifications(store, props), {
      initialProps: { ...sources, task_status: "Restarting ctl-taskd…" } as typeof sources,
    });
    hook.rerender({ ...sources, task_error: "Stop active tasks first." });
    expect(store.snapshot().entries).toMatchObject([{ severity: "error", message: "Stop active tasks first." }]);
    expect(store.snapshot().entries).toHaveLength(1);
  });

  it("reports storage failures even when no session is selected", () => {
    const store = new NotificationStore();
    renderHook(() => useWorkbenchNotifications(store, { ...sources, storage_error: "Archive failed: disk full" }));
    expect(store.snapshot().entries).toMatchObject([{ severity: "error", title: "Session history", message: "Archive failed: disk full" }]);
  });

  it.each(["ssh_authentication_required", "ssh_authentication_failed", "connection_timeout", "ctmux_connection_timeout", null])(
    "offers authentication only when the host inspection error requires it (%s)", (code) => {
      const store = new NotificationStore();
      const target = { kind: "ssh" as const, host_id: "fixture", destination: "workstation" };
      const key = targetKey(target);
      renderHook(() => useWorkbenchNotifications(store, {
        ...sources, targets: [target], target_errors: new Map([[key, { code, message: "Synthetic inspection failure" }]]),
      }));
      const authentication_required = code === "ssh_authentication_required" || code === "ssh_authentication_failed";
      expect(store.snapshot().entries).toMatchObject([{
        message: "Synthetic inspection failure",
        actions: [authentication_required
          ? { label: "Connect host", command_id: COMMAND_IDS.connectHost, args: { target_key: key } }
          : { label: "Refresh sessions", command_id: COMMAND_IDS.refreshSessions }],
      }]);
    },
  );

  it("does not claim recovery when a host error source disappears", () => {
    const store = new NotificationStore();
    const target = { kind: "ssh" as const, host_id: "fixture", destination: "workstation" };
    const key = targetKey(target);
    const hook = renderHook((props) => useWorkbenchNotifications(store, props), {
      initialProps: { ...sources, targets: [target], target_errors: new Map([[key, { code: "connection_timeout", message: "Synthetic timeout" }]]) } as typeof sources,
    });
    hook.rerender(sources);
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true, actions: [] }]);
  });

  it("does not replay an old ctl-taskd success after a later background error recovers", () => {
    const store = new NotificationStore();
    const initial = { ...sources, task_status: "ctl-taskd restarted." };
    const hook = renderHook((props) => useWorkbenchNotifications(store, props), { initialProps: initial as typeof sources });
    hook.rerender({ ...initial, task_error: "Task status unavailable" });
    store.clear();
    hook.rerender(initial);
    expect(store.snapshot().entries).toEqual([]);
    hook.rerender({ ...initial, task_error: "Task status unavailable" });
    expect(store.snapshot().entries).toMatchObject([{ severity: "error", message: "Task status unavailable" }]);
  });

  it("reports newly failed runs, skips historical failures at launch, and recognizes identical later failures", () => {
    const store = new NotificationStore();
    const task: ManagedTask = {
      task_id: "build", definition: { name: "Build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" },
      desired_state: "stopped", active_run: null,
      last_run: { run_id: "one", state: "failed", started_at_ms: 0, ended_at_ms: 1, exit_code: 1 },
    };
    const hook = renderHook((props) => useWorkbenchNotifications(store, props), {
      initialProps: { ...sources, tasks_loaded: true, tasks: [task] },
    });
    expect(store.snapshot().entries).toEqual([]);
    const failedAgain = { ...task, last_run: { ...task.last_run!, run_id: "two" } };
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [failedAgain] });
    expect(store.snapshot().entries).toMatchObject([{ title: "Task failed", occurrence_count: 1, actions: [{ args: { value: "build" } }] }]);
    store.hide(store.snapshot().entries[0].id);
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [{ ...failedAgain }] });
    expect(store.snapshot().entries[0].toast_visible).toBe(false);
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [{ ...task, last_run: { ...task.last_run!, run_id: "three" } }] });
    expect(store.snapshot().entries).toMatchObject([{ title: "Task failed", occurrence_count: 2, toast_visible: true }]);
  });
});
