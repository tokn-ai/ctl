// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { COMPONENT_RECONNECT_EVENT, COMPONENT_RESET_EVENT, useComponentActionEvents } from "./useComponentActionEvents";

const events = vi.hoisted(() => ({ listeners: new Map<string, (event: { payload: unknown }) => void>(), stop: vi.fn() }));
const api = vi.hoisted(() => ({ acknowledgeComponentReconnect: vi.fn(), reconnectComponentAttachments: vi.fn(), resetComponentAttachments: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => { events.listeners.set(name, callback); return events.stop; }) }));
vi.mock("../../lib/tauri", () => ({ acknowledgeComponentReconnect: api.acknowledgeComponentReconnect }));
vi.mock("../attachment/componentActions", () => ({ reconnectComponentAttachments: api.reconnectComponentAttachments, resetComponentAttachments: api.resetComponentAttachments }));
beforeEach(() => { vi.resetAllMocks(); events.listeners.clear(); api.acknowledgeComponentReconnect.mockResolvedValue(undefined); api.resetComponentAttachments.mockReturnValue([]); });
afterEach(cleanup);

it("acknowledges each native action once with exact replacement IDs and attachment failures", async () => {
  const results = [{ attachment_id: "one", replacement_attachment_id: "replacement", error: null }, { attachment_id: "two", replacement_attachment_id: null, error: "Gone" }];
  api.reconnectComponentAttachments.mockResolvedValue(results);
  renderHook(() => useComponentActionEvents(vi.fn()));
  act(() => {
    for (let attempt = 0; attempt < 2; attempt += 1) events.listeners.get(COMPONENT_RECONNECT_EVENT)!({ payload: { action_id: "action", attachment_ids: ["one", "two"] } });
  });
  await waitFor(() => expect(api.acknowledgeComponentReconnect).toHaveBeenCalledWith("action", results));
  expect(api.reconnectComponentAttachments).toHaveBeenCalledOnce();
  expect(api.resetComponentAttachments).not.toHaveBeenCalled();
});

it("clears sessions only after a native reset event and uses the latest window callback", async () => {
  const previous = vi.fn();
  const current = vi.fn();
  const hook = renderHook(({ callback }) => useComponentActionEvents(callback), { initialProps: { callback: previous } });
  expect(api.resetComponentAttachments).not.toHaveBeenCalled();
  hook.rerender({ callback: current });
  const payload = { scope: "remote", remote_id: "verified", host_ids: [], session_ids: [], attachment_ids: ["pane"] };
  act(() => events.listeners.get(COMPONENT_RESET_EVENT)!({ payload }));
  expect(api.resetComponentAttachments).toHaveBeenCalledWith(payload);
  expect(current).toHaveBeenCalledWith(payload, []);
  expect(previous).not.toHaveBeenCalled();
  await act(async () => {});
  hook.unmount();
  expect(events.stop).toHaveBeenCalledTimes(2);
});

it("surfaces acknowledgement failure without retrying a reconnect that may already have succeeded", async () => {
  api.reconnectComponentAttachments.mockResolvedValue([]);
  api.acknowledgeComponentReconnect.mockRejectedValue(new Error("Action expired."));
  const { result } = renderHook(() => useComponentActionEvents(vi.fn()));
  act(() => events.listeners.get(COMPONENT_RECONNECT_EVENT)!({ payload: { action_id: "expired", attachment_ids: [] } }));
  await waitFor(() => expect(result.current).toContain("Action expired."));
  expect(api.reconnectComponentAttachments).toHaveBeenCalledOnce();
});
