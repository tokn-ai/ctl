import { beforeEach, expect, it, vi } from "vitest";
import { getComponentVersions, preflightComponentAction, executeComponentAction, acknowledgeComponentReconnect } from "./tauri";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: ipc.invoke, Channel: class {} }));
beforeEach(() => vi.resetAllMocks());

it("keeps version observations read-only and restarts only with the native preflight token", async () => {
  await getComponentVersions();
  expect(ipc.invoke).toHaveBeenLastCalledWith("get_component_versions", undefined);
  await preflightComponentAction("ctld-selected-owner");
  expect(ipc.invoke).toHaveBeenLastCalledWith("preflight_component_action", { request: { component_id: "ctld-selected-owner" } });
  await executeComponentAction("native-preflight-token");
  expect(ipc.invoke).toHaveBeenLastCalledWith("execute_component_action", { request: { action_token: "native-preflight-token" } });
  const results = [{ attachment_id: "old", replacement_attachment_id: "new", error: null }];
  await acknowledgeComponentReconnect("action", results);
  expect(ipc.invoke).toHaveBeenLastCalledWith("ack_component_reconnect", { request: { action_id: "action", results } });
});
