import { beforeEach, expect, it, vi } from "vitest";
import { getAboutRuntimeStatus, syncAboutRuntime, getComponentVersions, preflightComponentAction, executeComponentAction, acknowledgeComponentReconnect, probeSshHost, getComponentBundles, selectComponentBundle, updateComponents, cancelComponentUpdate } from "./tauri";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: ipc.invoke, Channel: class {} }));
beforeEach(() => vi.resetAllMocks());

it("scopes runtime status and sync to the selected saved method", async () => {
  await getAboutRuntimeStatus({ kind: "local" });
  expect(ipc.invoke).toHaveBeenLastCalledWith("about_runtime_status", { host_id: null, method_id: null });
  const target = { kind: "ssh" as const, destination: "alias", host_id: "saved", method_id: "alternate" };
  await getAboutRuntimeStatus(target);
  expect(ipc.invoke).toHaveBeenLastCalledWith("about_runtime_status", { host_id: "saved", method_id: "alternate" });
  await syncAboutRuntime(target, "attempt");
  expect(ipc.invoke).toHaveBeenLastCalledWith("sync_about_runtime", { host_id: "saved", method_id: "alternate", attempt_id: "attempt" });
  ipc.invoke.mockClear();
  await expect(syncAboutRuntime({ kind: "ssh", destination: "alias" }, "attempt")).rejects.toThrow();
  await expect(syncAboutRuntime({ ...target, unavailable: "Route unavailable" }, "attempt")).rejects.toThrow("Route unavailable");
  expect(ipc.invoke).not.toHaveBeenCalled();
});

it("uses snake_case update requests and forwards authentication and progress channels", async () => {
  const request = { targets: [{ kind: "local" as const }, { kind: "ssh" as const, destination: "builder" }], attempt_id: "batch", options: { package: "ctl_agent" as const, source: { kind: "provided" as const, path: "/tmp/build", local_build: false, ctld_package: null } } };
  const prompt = vi.fn(), progress = vi.fn();
  await updateComponents(request, prompt, progress);
  expect(ipc.invoke).toHaveBeenLastCalledWith("update_components", { request, on_prompt: expect.any(Object), on_progress: expect.any(Object) });
  const channels = ipc.invoke.mock.lastCall![1];
  channels.on_prompt.onmessage({ prompt_id: "p" });
  channels.on_progress.onmessage({ host_index: 1, state: "updating" });
  expect(prompt).toHaveBeenCalledWith({ prompt_id: "p" });
  expect(progress).toHaveBeenCalledWith({ host_index: 1, state: "updating" });
  await cancelComponentUpdate("batch");
  expect(ipc.invoke).toHaveBeenLastCalledWith("cancel_component_update", { request: { attempt_id: "batch" } });
});

it("keeps version observations read-only and restarts only with the native preflight token", async () => {
  await getComponentVersions();
  expect(ipc.invoke).toHaveBeenLastCalledWith("get_component_versions", undefined);
  await getComponentVersions("selected-host");
  expect(ipc.invoke).toHaveBeenLastCalledWith("get_component_versions", { host_id: "selected-host" });
  await preflightComponentAction("ctld-selected-owner");
  expect(ipc.invoke).toHaveBeenLastCalledWith("preflight_component_action", { request: { component_id: "ctld-selected-owner" } });
  await executeComponentAction("native-preflight-token");
  expect(ipc.invoke).toHaveBeenLastCalledWith("execute_component_action", { request: { action_token: "native-preflight-token" } });
  const results = [{ attachment_id: "old", replacement_attachment_id: "new", error: null }];
  await acknowledgeComponentReconnect("action", results);
  expect(ipc.invoke).toHaveBeenLastCalledWith("ack_component_reconnect", { request: { action_id: "action", results } });
});

it("requests explicit component inspection independently of terminal probing", async () => {
  const target = { kind: "ssh" as const, destination: "saved" };
  await probeSshHost(target, "check", vi.fn(), true);
  expect(ipc.invoke).toHaveBeenLastCalledWith("probe_ssh_host", { request: { target, attempt_id: "check", components_only: true }, on_prompt: expect.any(Object) });
});


it("selects a complete build with snake_case fields and a progress channel", async () => {
  await getComponentBundles();
  expect(ipc.invoke).toHaveBeenLastCalledWith("get_component_bundles", undefined);
  const request = { bundle_id: "complete-build", target_triple: "aarch64-apple-darwin", purpose: "upload" as const };
  const progress = vi.fn();
  await selectComponentBundle(request, progress);
  expect(ipc.invoke).toHaveBeenLastCalledWith("select_component_bundle", { request, on_progress: expect.any(Object) });
  const channel = ipc.invoke.mock.calls[ipc.invoke.mock.calls.length - 1][1].on_progress;
  channel.onmessage("verifying");
  expect(progress).toHaveBeenCalledWith("verifying");
});
