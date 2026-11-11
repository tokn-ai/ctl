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
const history_ready: AttachmentViewState = {
  ...failed, phase: "attached", message: null, error_code: null, history_loading: false,
};

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

  it("offers an explicit reconnect when retry_wait is the first observable state", async () => {
    const { store, report, registry, owner, reconnect } = setup();
    report(transitionAttachment(failed, { type: "retry_scheduled", retry_at_ms: 1000 }));
    expect(store.snapshot().entries).toMatchObject([{ severity: "error", title: "Session connection failed",
      actions: [{ label: "Reconnect", command_id: COMMAND_IDS.reconnectNotificationAttachment, args: { value: owner } }],
    }]);
    expect(registry.canReconnect(owner)).toBe(true);
    await registry.reconnect(owner);
    expect(reconnect).toHaveBeenCalledOnce();
  });

  it("resolves an error only after attachment succeeds, then reports a later recurrence", () => {
    const { store, report } = setup();
    report(failed);
    report(transitionAttachment(failed, { type: "retry_scheduled", retry_at_ms: 1000 }));
    report({ ...connecting, phase: "reconnecting" });
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true, occurrence_count: 1 }]);
    report({ ...failed, phase: "attached", error_code: null, message: null });
    expect(store.snapshot().entries).toMatchObject([{
      resolved_at: expect.any(Number), toast_visible: false, actions: [], occurrence_count: 1,
    }]);
    report(failed);
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true, occurrence_count: 2 }]);
  });

  it("resolves a root open's earlier failure when the response supplies its pane identity", () => {
    const { store, report } = setup();
    report({ ...failed, session: { ...session, terminal_id: undefined } });
    report({ ...connecting, phase: "reconnecting", session: { ...session, terminal_id: undefined } });
    report({ ...failed, phase: "attached", error_code: null, message: null });
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: expect.any(Number), toast_visible: false }]);
  });

  it.each([
    { name: "session", previous_session: { ...session, terminal_id: undefined }, next_session: { ...session, session_id: "another-session" } },
    { name: "host", previous_session: { ...session, terminal_id: undefined }, next_session: { ...session, target: { kind: "ssh" as const, host_id: "another-host", destination: "workstation" } } },
    { name: "pane", previous_session: session, next_session: { ...session, terminal_id: "secondary" } },
  ])("does not resolve an earlier failure when its owner switches $name", ({ previous_session, next_session }) => {
    const { store, report } = setup();
    report({ ...failed, session: previous_session });
    report({ ...failed, session: next_session, phase: "attached", error_code: null, message: null });
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true, actions: [] }]);
  });

  it("does not resolve errors on explicit retry or source removal", async () => {
    const { store, registry, owner, report } = setup();
    report(failed);
    await registry.reconnect(owner);
    report({ ...connecting, phase: "reconnecting" });
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true }]);
    registry.remove(owner);
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true, actions: [] }]);
  });

  it("keeps attached warnings deduplicated instead of resolving them on every report", () => {
    const { store, report } = setup();
    const warning = { ...failed, phase: "attached" as const, error_code: null, message: "Another client controls this session's terminal size." };
    report(warning);
    report(warning);
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, occurrence_count: 1 }]);
  });

  it("resets deduplication when an attachment is closed and reopened", () => {
    const { store, report, registry, owner, reconnect } = setup();
    report(failed);
    store.dismiss(store.snapshot().entries[0].id);
    registry.remove(owner);
    const replacement = registry.createOwner();
    registry.report(replacement, connecting, reconnect);
    registry.report(replacement, failed, reconnect);
    expect(store.snapshot().entries).toMatchObject([{ toast_visible: true, actions: [{ args: { value: replacement } }] }]);
    expect(registry.canReconnect(owner)).toBe(false);
  });

  it("groups SSH authentication requests by host and retains the action until its sessions recover", () => {
    const { store, report, registry, reconnect } = setup();
    const target = { kind: "ssh" as const, host_id: "build", destination: "build.example" };
    const blocked = {
      ...failed, session: { ...session, target },
      error_code: "ssh_authentication_required", message: "Authentication needed.",
    };
    const second = { ...blocked, session: { ...blocked.session, session_id: "second", terminal_id: undefined } };
    const second_owner = registry.createOwner();
    const other_owner = registry.createOwner();
    report(blocked);
    registry.report(second_owner, second, reconnect);
    registry.report(other_owner, {
      ...blocked, session: { ...blocked.session, target: { ...target, host_id: "test", destination: "test.example" } },
    }, reconnect);
    const action = { label: "Connect host", command_id: COMMAND_IDS.connectHost, args: { target_key: "host:build" } };
    const build_notice = () => store.snapshot().entries.find((entry) => entry.source === "build.example");

    expect(store.snapshot().entries).toHaveLength(2);
    expect(store.snapshot().entries.every((entry) => !entry.toast_visible)).toBe(true);
    expect(build_notice()).toMatchObject({ occurrence_count: 1, actions: [action] });

    report({ ...blocked, phase: "attached", error_code: null, message: null });
    expect(build_notice()).toMatchObject({ resolved_at: null, actions: [action] });

    registry.report(second_owner, { ...second, phase: "reconnecting", error_code: null, message: null }, reconnect);
    expect(build_notice()).toMatchObject({ resolved_at: null });
    registry.report(second_owner, {
      ...second, session: { ...second.session, terminal_id: "new-pane" },
      phase: "attached", error_code: null, message: null,
    }, reconnect);
    expect(build_notice()).toMatchObject({ resolved_at: expect.any(Number), actions: [] });
    expect(store.snapshot().entries.find((entry) => entry.source === "test.example")).toMatchObject({ resolved_at: null });
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
    report({ ...history_ready, history_gap: true });
    store.dismiss(store.snapshot().entries[0].id);
    report(failed);
    report({ ...connecting, phase: "reconnecting" });
    report({ ...history_ready, history_gap: true });
    expect(store.snapshot().entries).toHaveLength(1);
    expect(store.snapshot().entries[0].actions).toEqual([]);
  });

  it.each([false, true])("does not warn while retained history is loading (gap=%s)", (history_gap) => {
    const { store, report } = setup();
    report({ ...history_ready, history_gap, history_loading: true });
    expect(store.snapshot().entries).toEqual([]);
  });

  it.each(["attached", "ended"] as const)("hides a warning during loading and resolves only confirmed %s recovery", (phase) => {
    const { store, report } = setup();
    report({ ...history_ready, history_gap: true });
    const id = store.snapshot().entries[0].id;
    expect(store.snapshot().entries).toMatchObject([{
      id, title: "Earlier output unavailable", toast_visible: true, resolved_at: null,
      message: "Some earlier output is unavailable in this view.",
    }]);

    report({ ...history_ready, history_gap: true, history_loading: true });
    expect(store.snapshot().entries).toMatchObject([{ id, toast_visible: false, resolved_at: null, occurrence_count: 1 }]);
    report({ ...history_ready, phase, history_gap: false, history_loading: true });
    expect(store.snapshot().entries[0].resolved_at).toBeNull();

    report({ ...history_ready, phase, history_gap: false });
    expect(store.snapshot().entries).toMatchObject([{ id, toast_visible: false, resolved_at: expect.any(Number), occurrence_count: 1 }]);

    report({ ...history_ready, phase, history_gap: true });
    expect(store.snapshot().entries).toMatchObject([{ id, toast_visible: true, resolved_at: null, occurrence_count: 2 }]);
  });

  it.each(["disconnected", "retry_wait", "reconnecting", "error"] as const)("does not treat a %s snapshot with no gap as confirmed recovery", (phase) => {
    const { store, report } = setup();
    report({ ...history_ready, history_gap: true });
    report({ ...history_ready, phase, history_gap: false });
    expect(store.snapshot().entries[0].resolved_at).toBeNull();
    report({ ...history_ready, phase, history_gap: false, history_loading: true });
    expect(store.snapshot().entries).toMatchObject([{ toast_visible: false, resolved_at: null }]);
  });

  it.each(["hide", "dismiss"] as const)("preserves history-warning %s and deduplication when loading finishes with the same gap", (action) => {
    const { store, report } = setup();
    report({ ...history_ready, history_gap: true });
    store[action](store.snapshot().entries[0].id);
    report({ ...connecting, phase: "reconnecting", history_gap: false, history_loading: true });
    report({ ...history_ready, history_gap: true });
    expect(store.snapshot().entries.filter((entry) => entry.toast_visible)).toEqual([]);
    if (action === "hide") expect(store.snapshot().entries).toMatchObject([{ occurrence_count: 1, resolved_at: null }]);
    else expect(store.snapshot().entries).toEqual([]);
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
    expect(store.snapshot().entries[0].resolved_at).toBeNull();
  });

  it("does not let a stale attached owner resolve a replacement pane's failure", () => {
    const { store, report, registry, reconnect } = setup();
    report(failed);
    const replacement = registry.createOwner();
    registry.report(replacement, failed, reconnect);
    report({ ...failed, phase: "attached", error_code: null, message: null });
    expect(registry.canReconnect(replacement)).toBe(true);
    expect(store.snapshot().entries).toMatchObject([{
      resolved_at: null, toast_visible: true, actions: [{ args: { value: replacement } }],
    }]);
  });

  it("survives StrictMode effect replay and releases actions on real unmount", async () => {
    const register = vi.spyOn(AttachmentNotifications.prototype, "createOwner");
    const store = new NotificationStore();
    const wrapper = ({ children }: { children: ReactNode }) => <StrictMode><NotificationProvider store={store}>{children}</NotificationProvider></StrictMode>;
    const hook = renderHook(() => {
      useAttachmentNotifications({ state: failed, reconnect: vi.fn(async () => {}), connection_attempt: 0 });
      return useNotificationEnvironment()!;
    }, { wrapper });
    await act(async () => {});
    expect(register).toHaveBeenCalledOnce();
    register.mockRestore();
    expect(store.snapshot().entries).toMatchObject([{ occurrence_count: 1 }]);
    const registry = hook.result.current.attachments;
    const owner = store.snapshot().entries[0].actions![0].args!.value;
    await act(async () => hook.unmount());
    expect(registry.canReconnect(owner)).toBe(false);
    expect(store.snapshot().entries[0].actions).toEqual([]);
  });
});
