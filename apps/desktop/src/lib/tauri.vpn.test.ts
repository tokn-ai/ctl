import { beforeEach, describe, expect, it, vi } from "vitest";
import { connectVpn, deleteVpnConnection, loadVpnConnections, saveVpnConnection, stopVpn, vpnStatus } from "./tauri";
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
    await stopVpn("work");
    expect(ipc.invoke).toHaveBeenLastCalledWith("stop_vpn", { request: { vpn_id: "work" } });
  });
});
