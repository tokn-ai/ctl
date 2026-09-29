import { beforeEach, expect, it, vi } from "vitest";
import { getComponentVersions, preflightRestartCtld, restartCtld } from "./tauri";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: ipc.invoke, Channel: class {} }));
beforeEach(() => vi.resetAllMocks());

it("keeps version observations read-only and restarts only with the native preflight token", async () => {
  await getComponentVersions();
  expect(ipc.invoke).toHaveBeenLastCalledWith("get_component_versions", undefined);
  await preflightRestartCtld("ctld-selected-owner");
  expect(ipc.invoke).toHaveBeenLastCalledWith("preflight_restart_ctld", { request: { component_id: "ctld-selected-owner" } });
  await restartCtld("native-preflight-token");
  expect(ipc.invoke).toHaveBeenLastCalledWith("restart_ctld", { request: { restart_token: "native-preflight-token" } });
});
