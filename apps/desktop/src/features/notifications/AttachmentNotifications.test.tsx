// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { StrictMode, type ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AttachmentViewState, SessionSummary } from "../../lib/types";
import { initialAttachmentState, transitionAttachment } from "../attachment/attachmentState";
import { COMMAND_IDS } from "../commands/commandIds";
import { AttachmentNotifications } from "./AttachmentNotifications";
import { NotificationProvider, useNotificationEnvironment } from "./NotificationContext";
import { NotificationStore } from "./NotificationStore";
import { useAttachmentNotifications } from "./useAttachmentNotifications";

afterEach(cleanup);

const session: SessionSummary = {
  target: { kind: "local" }, session_id: "shell", terminal_id: "primary", name: "shell", status: "running",
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null }, next_sequence: "0",
};
const connecting = transitionAttachment(initialAttachmentState(), {
  type: "begin", intent: "attach", session, resume_from: null, resize_with_window: false,
});
const failed = transitionAttachment(connecting, {
  type: "failed", code: "transport_error", message: "Connection interrupted.", resume_from: null,
});

function setup() {
  const store = new NotificationStore();
  const registry = new AttachmentNotifications(store);
  const owner = registry.createOwner();
  const reconnect = vi.fn(async () => {});
  const report = (state: AttachmentViewState) => registry.report(owner, state, reconnect);
  return { store, registry, owner, reconnect, report };
}

describe("attachment notification lifecycle", () => {
  it.each(["hide", "dismiss"] as const)("preserves %s through real automatic retry transitions", (dismiss) => {
    const { store, report } = setup();
    report(failed);
    store[dismiss](store.snapshot().entries[0].id);
    for (let attempt = 0; attempt < 3; attempt++) {
      report(transitionAttachment(failed, { type: "retry_scheduled", retry_at_ms: 1000 }));
      report({ ...connecting, phase: "reconnecting" });
      report(failed);
    }
    expect(store.snapshot().entries.filter((entry) => entry.toast_visible)).toEqual([]);
    expect(store.snapshot().entries).toHaveLength(dismiss === "hide" ? 1 : 0);
    if (dismiss === "hide") expect(store.snapshot().entries[0].occurrence_count).toBe(1);
    report(transitionAttachment(failed, { type: "retry_exhausted" }));
    expect(store.snapshot().entries[0]).toMatchObject({ title: "Session connection failed", toast_visible: true });
  });

  it("reports a failure when retry_wait is the first observable state", () => {
    const { store, report, registry, owner } = setup();
    report(transitionAttachment(failed, { type: "retry_scheduled", retry_at_ms: 1000 }));
    expect(store.snapshot().entries).toMatchObject([{ severity: "error", title: "Session connection failed", actions: [] }]);
    expect(registry.canReconnect(owner)).toBe(false);
  });

  it("resets deduplication when an attachment is closed and reopened", () => {
    const { store, report, registry, owner, reconnect } = setup();
    const authentication_failure = { ...failed, error_code: "ssh_authentication_required" };
    report(authentication_failure);
    store.dismiss(store.snapshot().entries[0].id);
    registry.remove(owner);
    const replacement = registry.createOwner();
    registry.report(replacement, connecting, reconnect);
    registry.report(replacement, authentication_failure, reconnect);
    expect(store.snapshot().entries).toMatchObject([{ toast_visible: true, actions: [{ args: { value: replacement } }] }]);
    expect(registry.canReconnect(owner)).toBe(false);
  });

  it("retries the specified pane using its latest callback and rejects stale actions", async () => {
    const { store, registry, owner, report, reconnect } = setup();
    report(failed);
    const pane_owner = registry.createOwner();
    const pane = { ...failed, session: { ...session, terminal_id: "secondary" } };
    const retry_pane = vi.fn(async () => {});
    registry.report(pane_owner, pane, retry_pane);
    expect(store.snapshot().entries).toHaveLength(2);
    const latest = vi.fn(async () => {});
    registry.report(pane_owner, pane, latest);
    const action = store.snapshot().entries[0].actions![0];
    expect(action.command_id).toBe(COMMAND_IDS.reconnectNotificationAttachment);
    await registry.reconnect(action.args?.value);
    expect(latest).toHaveBeenCalledOnce();
    expect(retry_pane).not.toHaveBeenCalled();
    expect(reconnect).not.toHaveBeenCalled();
    registry.remove(pane_owner);
    await registry.reconnect(action.args?.value);
    expect(latest).toHaveBeenCalledOnce();
    expect(registry.canReconnect(owner)).toBe(true);
    expect(store.snapshot().entries[0].actions).toEqual([]);
  });

  it("reports identical failures from an explicit retry or after recovery", async () => {
    const { store, report, registry, owner } = setup();
    report(failed);
    store.dismiss(store.snapshot().entries[0].id);
    await registry.reconnect(owner);
    report({ ...connecting, phase: "reconnecting" });
    report(failed);
    expect(store.snapshot().entries).toHaveLength(1);
    store.dismiss(store.snapshot().entries[0].id);
    report({ ...failed, phase: "attached", error_code: null, message: null });
    report(failed);
    expect(store.snapshot().entries).toHaveLength(1);
  });

  it("retains pane history warnings across retry snapshots and clears obsolete recovery actions", () => {
    const { store, report } = setup();
    report({ ...failed, phase: "attached", message: null, history_gap: true });
    store.dismiss(store.snapshot().entries[0].id);
    report(failed);
    report({ ...connecting, phase: "reconnecting" });
    report({ ...failed, phase: "attached", message: null, history_gap: true });
    expect(store.snapshot().entries).toHaveLength(1);
    expect(store.snapshot().entries[0].actions).toEqual([]);
  });

  it.each([session, { ...session, target: { kind: "ssh" as const, host_id: "remote", destination: "workstation" } }])(
    "retains confirmed component recovery for $target.kind panes", (session) => {
      const { store, registry, owner, report } = setup();
      report({ ...failed, session, error_code: "protocol_version_mismatch" });
      expect(store.snapshot().entries[0].actions).toMatchObject([{ command_id: COMMAND_IDS.recoverSessionComponents }]);
      expect(registry.recoverySession(owner)).toBe(session);
      expect(registry.canReconnect(owner)).toBe(false);
      report({ ...failed, session, phase: "attached", error_code: null, message: null });
      expect(registry.recoverySession(owner)).toBeNull();
      expect(store.snapshot().entries[0].actions).toEqual([]);
    },
  );

  it("does not let a retiring owner clear a replacement pane's failure", () => {
    const { store, report, registry, owner, reconnect } = setup();
    report(failed);
    const replacement = registry.createOwner();
    registry.report(replacement, failed, reconnect);
    registry.remove(owner);
    expect(registry.canReconnect(replacement)).toBe(true);
    expect(store.snapshot().entries[0].actions).toMatchObject([{ args: { value: replacement } }]);
  });

  it("survives StrictMode effect replay and releases actions on real unmount", async () => {
    const store = new NotificationStore();
    const wrapper = ({ children }: { children: ReactNode }) => <StrictMode><NotificationProvider store={store}>{children}</NotificationProvider></StrictMode>;
    const hook = renderHook(() => {
      useAttachmentNotifications({ state: failed, reconnect: vi.fn(async () => {}), connection_attempt: 0 });
      return useNotificationEnvironment()!;
    }, { wrapper });
    await act(async () => {});
    expect(store.snapshot().entries).toMatchObject([{ occurrence_count: 1 }]);
    const registry = hook.result.current.attachments;
    const owner = store.snapshot().entries[0].actions![0].args!.value;
    await act(async () => hook.unmount());
    expect(registry.canReconnect(owner)).toBe(false);
    expect(store.snapshot().entries[0].actions).toEqual([]);
  });
});
