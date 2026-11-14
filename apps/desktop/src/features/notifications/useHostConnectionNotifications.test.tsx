// @vitest-environment jsdom
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { HostConnectionStatus, SshConnectionTarget } from "../../lib/types";
import { message } from "../i18n/messages";
import { NotificationStore } from "./NotificationStore";
import { useHostConnectionNotifications } from "./useHostConnectionNotifications";

const target: SshConnectionTarget = { kind: "ssh", host_id: "remote", destination: "example", host_name: "Example" };
function status(availability: "available" | "unavailable" | "unknown", overrides: Partial<HostConnectionStatus> = {}): HostConnectionStatus {
  return {
    state: availability === "available" ? "connected" : "disconnected", method_names: [], message: null,
    observation: { availability, completeness: "complete", method_names: [], failed_method_names: [], message: null, checked_at_ms: 1, stale: false },
    ...overrides,
  };
}
function observe(store: NotificationStore, initial_status: HostConnectionStatus) {
  return renderHook(({ connection, attached_host_keys }) => useHostConnectionNotifications(store, [target], new Map([["remote", connection]]), attached_host_keys), {
    initialProps: { connection: initial_status, attached_host_keys: new Set<string>() },
  });
}
afterEach(cleanup);

describe("host connection notification ownership", () => {
  it("reports only a new proven loss and offers the host connection action", () => {
    const store = new NotificationStore();
    const hook = observe(store, status("unavailable"));
    expect(store.snapshot().entries).toHaveLength(0);
    hook.rerender({ connection: status("available"), attached_host_keys: new Set() });
    hook.rerender({ connection: status("unavailable"), attached_host_keys: new Set() });
    expect(store.snapshot().entries).toMatchObject([{
      source_key: "host-connection:remote", title: message("notifications.ssh_lost"), occurrence_count: 1,
      actions: [{ label: message("shell.connect_host"), args: { target_key: "host:remote" } }],
    }]);
    const entry = store.snapshot().entries[0];
    store.hide(entry.id);
    hook.rerender({ connection: status("unavailable", { message: "Still unavailable" }), attached_host_keys: new Set() });
    expect(store.snapshot().entries[0]).toMatchObject({ occurrence_count: 1, toast_visible: false });
    hook.rerender({ connection: status("available"), attached_host_keys: new Set() });
    expect(store.snapshot().entries[0].resolved_at).not.toBeNull();
    hook.rerender({ connection: status("unavailable"), attached_host_keys: new Set() });
    expect(store.snapshot().entries[0]).toMatchObject({ occurrence_count: 2, toast_visible: true });
  });

  it("keeps unknown, stale, partial, and failed observations local", () => {
    const store = new NotificationStore();
    const hook = observe(store, status("available"));
    for (const observation of [
      status("unknown").observation!,
      { ...status("unavailable").observation!, stale: true },
      { ...status("unavailable").observation!, completeness: "partial" as const },
      { ...status("unavailable").observation!, completeness: "failed" as const },
    ]) hook.rerender({ connection: status("unavailable", { observation }), attached_host_keys: new Set() });
    expect(store.snapshot().entries).toHaveLength(0);
    hook.rerender({ connection: status("unavailable"), attached_host_keys: new Set() });
    expect(store.snapshot().entries).toHaveLength(1);
  });

  it.each(["manual", "pending", "failed"])("does not interpret a %s operation as unexpected connection loss", (reason) => {
    const store = new NotificationStore();
    const hook = observe(store, status("available"));
    hook.rerender({ connection: status("unavailable", reason === "manual"
      ? { manually_disconnected: true }
      : { operation: { kind: "disconnect", state: reason === "failed" ? "failed" : "pending", method_name: null, message: null } }), attached_host_keys: new Set() });
    hook.rerender({ connection: status("unavailable"), attached_host_keys: new Set() });
    expect(store.snapshot().entries).toHaveLength(0);
  });

  it("leaves attached hosts to their terminal owner without a delayed duplicate", () => {
    const store = new NotificationStore();
    const hook = observe(store, status("available"));
    hook.rerender({ connection: status("unavailable"), attached_host_keys: new Set(["remote"]) });
    hook.rerender({ connection: status("unavailable"), attached_host_keys: new Set() });
    expect(store.snapshot().entries).toHaveLength(0);
    hook.rerender({ connection: status("available"), attached_host_keys: new Set() });
    hook.rerender({ connection: status("unavailable"), attached_host_keys: new Set() });
    expect(store.snapshot().entries).toHaveLength(1);
  });
});
