import { describe, expect, it, vi } from "vitest";
import type { NotificationInput } from "../../lib/types";
import { MAX_NOTIFICATIONS, MAX_NOTIFICATION_TOASTS, NotificationStore } from "./NotificationStore";

const failure: NotificationInput = {
  severity: "error", title: "Connection failed", message: "Host unreachable", source: "workstation",
};

describe("window notification history", () => {
  it("keeps hidden cards for review, marks them read on review, and removes dismissed cards", () => {
    const store = new NotificationStore();
    store.report("host", failure);
    const { id } = store.snapshot().entries[0];
    store.hide(id);
    expect(store.snapshot().entries).toMatchObject([{ id, toast_visible: false, read: false }]);
    store.setCenterOpen(true);
    expect(store.snapshot().entries).toMatchObject([{ id, toast_visible: false, read: true }]);
    store.setCenterOpen(false);
    expect(store.snapshot().entries[0].toast_visible).toBe(false);
    store.dismiss(id);
    expect(store.snapshot().entries).toEqual([]);
  });

  it("does not resurrect dismissed failures on rerender, but reports new failures after recovery", () => {
    const store = new NotificationStore();
    const changed = vi.fn();
    store.subscribe(changed);
    store.report("host", failure);
    store.clear();
    changed.mockClear();
    store.report("host", { ...failure });
    expect(changed).not.toHaveBeenCalled();
    store.report("host", null);
    store.report("host", failure);
    expect(store.snapshot().entries).toMatchObject([{ ...failure, read: false, toast_visible: true }]);
  });

  it("groups a recurring failure and replaces progress with its final outcome", () => {
    let time = 1;
    const store = new NotificationStore(() => time++);
    store.report("host", failure);
    store.setCenterOpen(true);
    store.setCenterOpen(false);
    store.report("host", null);
    store.report("host", failure);
    expect(store.snapshot().entries).toMatchObject([{ occurrence_count: 2, read: false, toast_visible: true, created_at: 1, updated_at: 2 }]);
    store.report("host", { ...failure, severity: "success", message: "Connected" });
    expect(store.snapshot().entries).toMatchObject([{ occurrence_count: 1, severity: "success", message: "Connected" }]);
  });

  it("updates actions without counting or showing the same error again", () => {
    const store = new NotificationStore();
    store.report("host", failure);
    store.hide(store.snapshot().entries[0].id);
    store.report("host", { ...failure, actions: [{ label: "Retry", command_id: "host.connect" }] });
    expect(store.snapshot().entries).toMatchObject([{ occurrence_count: 1, toast_visible: false, actions: [{ label: "Retry" }] }]);
  });

  it("announces identical distinct operations again after hiding or dismissing the earlier event", () => {
    const store = new NotificationStore();
    store.notify("copy", failure);
    const id = store.snapshot().entries[0].id;
    store.setCenterOpen(true);
    store.setCenterOpen(false);
    store.notify("copy", failure);
    expect(store.snapshot().entries).toMatchObject([{ id, occurrence_count: 2, read: false, toast_visible: true }]);
    store.dismiss(id);
    store.notify("copy", failure);
    expect(store.snapshot().entries).toMatchObject([{ occurrence_count: 1, read: false, toast_visible: true }]);
  });

  it("can record a distinct operation silently without a popup", () => {
    const store = new NotificationStore();
    store.notify("workspace", failure, { toast: false });
    store.notify("workspace", failure, { toast: false });
    expect(store.snapshot().entries).toMatchObject([{ occurrence_count: 2, toast_visible: false }]);
  });

  it.each(["hide", "review"] as const)("preserves %s and occurrence count when one incident changes diagnostics", (action) => {
    let time = 1;
    const store = new NotificationStore(() => time++);
    store.report("host", failure);
    const id = store.snapshot().entries[0].id;
    if (action === "hide") store.hide(id);
    else {
      store.setCenterOpen(true);
      store.setCenterOpen(false);
    }
    const updated = { ...failure, message: "Retry timed out.", actions: [{ label: "Retry", command_id: "host.connect" }] };
    store.report("host", updated, { preserve_attention: true });
    expect(store.snapshot().entries).toMatchObject([{
      ...updated, id, created_at: 1, updated_at: 2, occurrence_count: 1,
      read: action === "review", toast_visible: false, resolved_at: null,
    }]);
    store.report("host", { ...updated, severity: "warning", message: "Automatic retries paused." }, { preserve_attention: true });
    expect(store.snapshot().entries).toMatchObject([{
      id, occurrence_count: 1, read: action === "review", toast_visible: false, updated_at: 3,
    }]);
  });

  it.each(["dismiss", "clear"] as const)("preserves %s across changing diagnostics until recovery or a distinct operation", (action) => {
    const store = new NotificationStore();
    store.report("host", failure);
    if (action === "dismiss") store.dismiss(store.snapshot().entries[0].id);
    else store.clear();
    const updated = { ...failure, message: "Automatic retries paused." };
    store.report("host", updated, { preserve_attention: true });
    store.report("host", { ...updated, message: "The next retry also failed." }, { preserve_attention: true });
    expect(store.snapshot().entries).toEqual([]);
    store.resolve("host");
    store.report("host", updated, { preserve_attention: true });
    expect(store.snapshot().entries).toMatchObject([{ message: updated.message, occurrence_count: 1, toast_visible: true }]);
  });

  it("resolves an error into history and treats a later identical failure as active", () => {
    let time = 1;
    const store = new NotificationStore(() => time++);
    store.report("host", { ...failure, actions: [{ label: "Retry", command_id: "host.connect" }] });
    store.resolve("host");
    expect(store.snapshot().entries).toMatchObject([{
      ...failure, created_at: 1, updated_at: 1, resolved_at: 2,
      occurrence_count: 1, toast_visible: false, actions: [],
    }]);
    const snapshot = store.snapshot();
    store.resolve("host");
    expect(store.snapshot()).toBe(snapshot);
    store.report("host", failure);
    expect(store.snapshot().entries).toMatchObject([{
      ...failure, created_at: 1, updated_at: 3, resolved_at: null,
      occurrence_count: 2, toast_visible: true,
    }]);
  });

  it("does not mistake source removal or a new attempt for proven recovery", () => {
    const store = new NotificationStore();
    store.report("host", failure);
    store.report("host", null);
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true }]);
  });

  it("does not restore a dismissed occurrence when it resolves", () => {
    const store = new NotificationStore();
    store.report("host", failure);
    store.dismiss(store.snapshot().entries[0].id);
    store.resolve("host");
    expect(store.snapshot().entries).toEqual([]);
    store.report("host", failure);
    expect(store.snapshot().entries).toMatchObject([{ resolved_at: null, toast_visible: true }]);
  });

  it("bounds visible cards and history, and starts empty in a new window", () => {
    const store = new NotificationStore();
    for (let index = 0; index < MAX_NOTIFICATIONS + 10; index++) store.report(`host:${index}`, failure);
    const entries = store.snapshot().entries;
    expect(entries).toHaveLength(MAX_NOTIFICATIONS);
    expect(entries.filter((entry) => entry.toast_visible)).toHaveLength(MAX_NOTIFICATION_TOASTS);
    expect(entries[0].source_key).toBe(`host:${MAX_NOTIFICATIONS + 9}`);
    expect(new NotificationStore().snapshot()).toEqual({ entries: [], center_open: false });
  });

  it("adds notifications directly to an open center without an unread badge or toast", () => {
    const store = new NotificationStore();
    store.setCenterOpen(true);
    store.report("host", failure);
    expect(store.snapshot().entries).toMatchObject([{ read: true, toast_visible: false }]);
    store.clear();
    store.report("host", failure);
    expect(store.snapshot().entries).toEqual([]);
  });
});
