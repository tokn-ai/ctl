// @vitest-environment jsdom
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ManagedTask, SessionSummary } from "../../lib/types";
import { initialAttachmentState } from "../attachment/attachmentState";
import { AttachmentNotifications } from "./AttachmentNotifications";
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

function renderNotifications(store: NotificationStore, initialProps: typeof sources, attachments = new AttachmentNotifications(store)) {
  return renderHook((props) => useWorkbenchNotifications(store, props, attachments), { initialProps });
}

describe("workbench notification sources", () => {
  it("updates a progress card with failure instead of leaving a stale operation running", () => {
    const store = new NotificationStore();
    const hook = renderNotifications(store, { ...sources, task_status: "Restarting ctl-taskd…" });
    hook.rerender({ ...sources, task_error: "Stop active tasks first." });
    expect(store.snapshot().entries).toMatchObject([{ severity: "error", message: "Stop active tasks first." }]);
    expect(store.snapshot().entries).toHaveLength(1);
  });

  it("reports storage failures even when no session is selected", () => {
    const store = new NotificationStore();
    renderNotifications(store, { ...sources, storage_error: "Archive failed: disk full" });
    expect(store.snapshot().entries).toMatchObject([{ severity: "error", title: "Session history", message: "Archive failed: disk full" }]);
  });

  it.each(["ssh_authentication_required", "ssh_authentication_failed", "connection_timeout", "ctmux_connection_timeout", null])(
    "offers authentication only when the host inspection error requires it (%s)", (code) => {
      const store = new NotificationStore();
      const target = { kind: "ssh" as const, host_id: "fixture", destination: "workstation" };
      const key = targetKey(target);
      renderNotifications(store, {
        ...sources, targets: [target], target_errors: new Map([[key, { code, message: "Synthetic inspection failure" }]]),
      });
      const authentication_required = code === "ssh_authentication_required" || code === "ssh_authentication_failed";
      expect(store.snapshot().entries).toMatchObject([{
        message: code === "ssh_authentication_required" ? "Use Connect host to authenticate." : "Synthetic inspection failure",
        toast_visible: code !== "ssh_authentication_required",
        actions: [authentication_required
          ? { label: "Connect host", command_id: COMMAND_IDS.connectHost, args: { target_key: key } }
          : { label: "Refresh sessions", command_id: COMMAND_IDS.refreshSessions }],
      }]);
    },
  );

  it("shares the host authentication notice with sessions waiting to connect", () => {
    const store = new NotificationStore();
    const attachments = new AttachmentNotifications(store);
    const target = { kind: "ssh" as const, host_id: "fixture", destination: "workstation" };
    const key = targetKey(target);
    const code = "ssh_authentication_required";
    const message = "Authentication needed.";
    const session: SessionSummary = {
      target, session_id: "session", name: "shell", status: "running", next_sequence: "0",
      terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
    };
    const blocked = { ...initialAttachmentState(), phase: "error" as const, session, error_code: code, message };
    const owner = attachments.createOwner();
    const reconnect = async () => {};
    attachments.report(owner, blocked, reconnect);
    const hook = renderNotifications(store, {
      ...sources, targets: [target], target_errors: new Map([[key, { code, message }]]),
    }, attachments);

    const notice = {
      title: "SSH authentication required", toast_visible: false, resolved_at: null, occurrence_count: 1,
      actions: [{ label: "Connect host", command_id: COMMAND_IDS.connectHost, args: { target_key: key } }],
    };
    expect(store.snapshot().entries).toMatchObject([notice]);
    hook.rerender({ ...sources, targets: [target] });
    expect(store.snapshot().entries).toMatchObject([notice]);

    attachments.report(owner, { ...blocked, phase: "attached", error_code: null, message: null }, reconnect);
    expect(store.snapshot().entries).toMatchObject([{ ...notice, resolved_at: expect.any(Number), actions: [] }]);
  });

  it("does not claim recovery when a host error source disappears", () => {
    const store = new NotificationStore();
    const target = { kind: "ssh" as const, host_id: "fixture", destination: "workstation" };
    const key = targetKey(target);
    const hook = renderNotifications(store, {
      ...sources, targets: [target], target_errors: new Map([[key, { code: "connection_timeout", message: "Synthetic timeout" }]]),
    });
    hook.rerender(sources);
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true, actions: [] }]);
  });

  it("does not replay an old ctl-taskd success after a later background error recovers", () => {
    const store = new NotificationStore();
    const initial = { ...sources, task_status: "ctl-taskd restarted." };
    const hook = renderNotifications(store, initial);
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
    const hook = renderNotifications(store, { ...sources, tasks_loaded: true, tasks: [task] });
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
