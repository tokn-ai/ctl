// @vitest-environment jsdom
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ManagedTask } from "../../lib/types";
import { formatText } from "../i18n/messages";
import { NotificationStore } from "./NotificationStore";
import { useWorkbenchNotifications } from "./useWorkbenchNotifications";
import { COMMAND_IDS } from "../commands/commandIds";

afterEach(cleanup);

const sources: Parameters<typeof useWorkbenchNotifications>[1] = {
  workspace_error: null, keybindings_error: null, storage_error: null, window_error: null,
  tasks: [], tasks_loaded: false,
};

function renderNotifications(store: NotificationStore, initialProps: typeof sources) {
  return renderHook((props) => useWorkbenchNotifications(store, props), { initialProps });
}

function englishEntries(store: NotificationStore) {
  return store.snapshot().entries.map((entry) => ({
    ...entry,
    title: formatText("en", entry.title),
    message: formatText("en", entry.message),
    actions: entry.actions?.map((action) => ({ ...action, label: formatText("en", action.label) })),
  }));
}

const failedTask: ManagedTask = {
  task_id: "build", definition: { name: "Build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" },
  desired_state: "stopped", active_run: null,
  last_run: { run_id: "one", state: "failed", started_at_ms: 0, ended_at_ms: 1, exit_code: 1 },
};

describe("workbench notification sources", () => {
  it("records workspace save failures in the notification center with a retry action", () => {
    const store = new NotificationStore();
    const hook = renderNotifications(store, { ...sources, workspace_error: "Workspace could not be saved." });
    expect(englishEntries(store)).toMatchObject([{
      title: "Workspace", message: "Workspace could not be saved.", severity: "error", toast_visible: false,
      actions: [{ label: "Retry saving", command_id: COMMAND_IDS.saveWorkspace }],
    }]);
    hook.rerender(sources);
    expect(englishEntries(store)).toMatchObject([{ resolved_at: expect.any(Number), toast_visible: false, actions: [] }]);
  });

  it("reports shortcut failures with retained-bindings context and a reload action", () => {
    const store = new NotificationStore();
    const initial = { ...sources, keybindings_error: "Invalid shortcut document." };
    const hook = renderNotifications(store, initial);
    expect(englishEntries(store)).toMatchObject([{
      title: "Keyboard shortcuts", severity: "error", toast_visible: true,
      message: "Keyboard shortcuts: Invalid shortcut document. Last valid bindings remain active.",
      actions: [{ label: "Reload shortcuts", command_id: COMMAND_IDS.reloadKeybindings }],
    }]);
    store.dismiss(store.snapshot().entries[0].id);
    hook.rerender({ ...initial });
    expect(store.snapshot().entries).toEqual([]);
    hook.rerender(sources);
    hook.rerender(initial);
    expect(store.snapshot().entries).toHaveLength(1);
  });

  it("reports independent storage and window failures without a selected session", () => {
    const store = new NotificationStore();
    const hook = renderNotifications(store, {
      ...sources, storage_error: "Archive failed: disk full", window_error: "Window could not be closed.",
    });
    expect(englishEntries(store)).toMatchObject([
      { severity: "error", title: "Window", message: "Window could not be closed.", toast_visible: true },
      { severity: "error", title: "Session history", message: "Archive failed: disk full", toast_visible: true },
    ]);
    hook.rerender(sources);
    expect(store.snapshot().entries.every((entry) => entry.resolved_at !== null && !entry.toast_visible)).toBe(true);
  });

  it("reports newly failed runs, skips historical failures at launch, and recognizes identical later failures", () => {
    const store = new NotificationStore();
    const hook = renderNotifications(store, { ...sources, tasks_loaded: true, tasks: [failedTask] });
    expect(store.snapshot().entries).toEqual([]);
    const failedAgain = { ...failedTask, last_run: { ...failedTask.last_run!, run_id: "two" } };
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [failedAgain] });
    expect(englishEntries(store)).toMatchObject([{
      title: "Task failed", occurrence_count: 1, message: "Build failed with exit code 1.",
      actions: [{ label: "View task", command_id: COMMAND_IDS.openTask, args: { value: "build" } }],
    }]);
    store.hide(store.snapshot().entries[0].id);
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [{ ...failedAgain }] });
    expect(store.snapshot().entries[0].toast_visible).toBe(false);
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [{ ...failedTask, last_run: { ...failedTask.last_run!, run_id: "three" } }] });
    expect(englishEntries(store)).toMatchObject([{ title: "Task failed", occurrence_count: 2, toast_visible: true }]);
  });

  it("does not mistake unloaded historical tasks or normal completion for a new failure", () => {
    const store = new NotificationStore();
    const hook = renderNotifications(store, { ...sources, tasks: [failedTask] });
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [failedTask] });
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [{ ...failedTask, last_run: { ...failedTask.last_run!, run_id: "two", state: "completed", exit_code: 0 } }] });
    expect(store.snapshot().entries).toEqual([]);
  });

  it("announces a running task's failure once and preserves dismissal until another run fails", () => {
    const store = new NotificationStore();
    const running: ManagedTask = {
      ...failedTask, last_run: null,
      active_run: { ...failedTask.last_run!, state: "running", ended_at_ms: null, exit_code: null },
    };
    const hook = renderNotifications(store, { ...sources, tasks_loaded: true, tasks: [running] });
    const failed = { ...failedTask, last_run: { ...failedTask.last_run!, exit_code: null } };
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [failed] });
    expect(englishEntries(store)).toMatchObject([{ title: "Task failed", message: "Build failed." }]);
    store.dismiss(store.snapshot().entries[0].id);
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [{ ...failed }] });
    expect(store.snapshot().entries).toEqual([]);
    hook.rerender({ ...sources, tasks_loaded: true, tasks: [{ ...failed, last_run: { ...failed.last_run!, run_id: "another" } }] });
    expect(englishEntries(store)).toMatchObject([{ title: "Task failed", toast_visible: true }]);
  });
});
