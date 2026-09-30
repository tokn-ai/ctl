// @vitest-environment jsdom
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { AttachmentViewState, ManagedTask, SessionSummary } from "../../lib/types";
import { NotificationStore } from "./NotificationStore";
import { useWorkbenchNotifications } from "./useWorkbenchNotifications";

afterEach(cleanup);

const session: SessionSummary = {
  target: { kind: "local" }, session_id: "shell", name: "shell", status: "running",
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
  next_sequence: "0",
};
const attachment: AttachmentViewState = {
  phase: "idle", session: null, attachment_id: null, error_code: null, message: null,
  input_lease: { held: false, owned_by_client: false }, layout_lease: { held: false, owned_by_client: false },
  shell_state: null, applied_sequence: null, reconnect_sequence: null,
  history_gap: false, terminal_size_mismatch: false, resize_with_window: false,
};
const sources: Parameters<typeof useWorkbenchNotifications>[1] = {
  workspace_error: null, workspace_ready: true, keybindings_error: null, session_error: null,
  targets: [], target_errors: new Map(), attachment,
  task_error: null, definitions_error: null, task_status: null, tasks: [], tasks_loaded: false,
};

describe("workbench notification sources", () => {
  it("updates a progress card with failure instead of leaving a stale operation running", () => {
    const store = new NotificationStore();
    const hook = renderHook((props) => useWorkbenchNotifications(store, props), {
      initialProps: { ...sources, task_status: "Restarting taskd…" } as typeof sources,
    });
    hook.rerender({ ...sources, task_error: "Stop active tasks first." });
    expect(store.snapshot().entries).toMatchObject([{ severity: "error", message: "Stop active tasks first." }]);
    expect(store.snapshot().entries).toHaveLength(1);
  });

  it("does not flood history during repeated attachment retries", () => {
    const store = new NotificationStore();
    const failed = { ...sources, attachment: { ...attachment, session, phase: "error" as const, message: "Connection lost" } };
    const hook = renderHook((props) => useWorkbenchNotifications(store, props), { initialProps: failed as typeof sources });
    store.dismiss(store.snapshot().entries[0].id);
    hook.rerender({ ...sources, attachment: { ...attachment, session, phase: "reconnecting" } });
    hook.rerender(failed);
    expect(store.snapshot().entries).toEqual([]);
    hook.rerender({ ...sources, attachment: { ...attachment, session, phase: "attached" } });
    hook.rerender(failed);
    expect(store.snapshot().entries).toMatchObject([{ title: "Session connection failed", toast_visible: true }]);
  });

  it("does not replay an old taskd success after a later background error recovers", () => {
    const store = new NotificationStore();
    const initial = { ...sources, task_status: "taskd restarted." };
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
