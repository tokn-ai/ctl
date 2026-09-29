import { beforeEach, describe, expect, it, vi } from "vitest";
import { beginVpnEnrollment, cancelVpnEnrollment, saveVpnEnrollment, vpnEnrollmentStatus, connectVpn, deleteVpnConnection, loadVpnConnections, openVpnSignIn, saveVpnConnection, stopVpn, vpnStatus } from "./tauri";
import type { VpnConnectionInput } from "./types";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: ipc.invoke,
  Channel: class { onmessage = (_message: unknown) => {}; },
}));

beforeEach(() => vi.resetAllMocks());

describe("VPN native boundary", () => {
  it("sends edits with an optimistic revision and explicit password preservation", async () => {
    const connection: VpnConnectionInput = {
      connection_id: "work", name: "Work", url: "https://vpn.example.com",
      username: "example", password: null, auth_method: null, target_ip: null,
    };
    await saveVpnConnection("revision", connection);
    expect(ipc.invoke).toHaveBeenCalledWith("save_vpn_connection", {
      request: { expected_revision: "revision", connection },
    });
    await deleteVpnConnection("revision", "work");
    expect(ipc.invoke).toHaveBeenLastCalledWith("delete_vpn_connection", {
      request: { expected_revision: "revision", connection_id: "work" },
    });
  });

  it("connects by saved identity without sending credentials from the webview", async () => {
    await loadVpnConnections();
    expect(ipc.invoke).toHaveBeenLastCalledWith("load_vpn_connections", undefined);
    await connectVpn("work");
    expect(ipc.invoke).toHaveBeenLastCalledWith("connect_vpn", { request: { connection_id: "work" } });
    await vpnStatus();
    expect(ipc.invoke).toHaveBeenLastCalledWith("vpn_status", undefined);
    await openVpnSignIn("work");
    expect(ipc.invoke).toHaveBeenLastCalledWith("open_vpn_sign_in", { request: { vpn_id: "work" } });
    await stopVpn("work");
    expect(ipc.invoke).toHaveBeenLastCalledWith("stop_vpn", { request: { vpn_id: "work" } });
  });
});


it("enrolls, observes, saves, and cancels through scoped native IDs", async () => {
  await beginVpnEnrollment({ name: "Tailnet", hostname: null, accept_routes: false });
  expect(ipc.invoke).toHaveBeenLastCalledWith("begin_vpn_enrollment", { request: { name: "Tailnet", hostname: null, accept_routes: false } });
  await vpnEnrollmentStatus("draft-one");
  expect(ipc.invoke).toHaveBeenLastCalledWith("vpn_enrollment_status", { request: { enrollment_id: "draft-one" } });
  await saveVpnEnrollment("draft-one", "revision");
  expect(ipc.invoke).toHaveBeenLastCalledWith("save_vpn_enrollment", { request: { enrollment_id: "draft-one", expected_revision: "revision" } });
  await cancelVpnEnrollment("draft-one");
  expect(ipc.invoke).toHaveBeenLastCalledWith("cancel_vpn_enrollment", { request: { enrollment_id: "draft-one" } });
});
