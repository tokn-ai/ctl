// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { VpnConnection, VpnConnectionInput, VpnConnectionsSnapshot, VpnStatus } from "../../lib/types";
import { connectVpn, deleteVpnConnection, loadVpnConnections, saveVpnConnection, stopVpn, vpnStatus } from "../../lib/tauri";
import { useVpn } from "./useVpn";

vi.mock("../../lib/tauri", () => ({
  connectVpn: vi.fn(), deleteVpnConnection: vi.fn(), loadVpnConnections: vi.fn(),
  saveVpnConnection: vi.fn(), stopVpn: vi.fn(), vpnStatus: vi.fn(),
}));

const connection: VpnConnection = {
  connection_id: "work", name: "Work", url: "https://vpn.example.test", username: "example-user",
  has_password: true, auth_method: null, target_ip: null,
};
const snapshot: VpnConnectionsSnapshot = { revision: "revision-1", connections: [connection] };
const stopped: VpnStatus = { endpoint: null, container_name: null, connection_id: null, running: false, state: "stopped" };
const connected: VpnStatus = {
  endpoint: "socks5h://127.0.0.1:49152", container_name: "test-vpn", connection_id: connection.connection_id,
  running: true, state: "connected",
};
const input: VpnConnectionInput = {
  connection_id: connection.connection_id, name: "Updated work", url: connection.url, username: connection.username,
  password: null, auth_method: null, target_ip: null,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(loadVpnConnections).mockResolvedValue(snapshot);
  vi.mocked(vpnStatus).mockResolvedValue(stopped);
  vi.mocked(connectVpn).mockResolvedValue(connected);
  vi.mocked(stopVpn).mockResolvedValue(stopped);
  vi.mocked(saveVpnConnection).mockResolvedValue({ revision: "revision-2", connections: [{ ...connection, name: input.name }] });
  vi.mocked(deleteVpnConnection).mockResolvedValue({ revision: "revision-2", connections: [] });
});
afterEach(cleanup);

async function ready(result: { current: ReturnType<typeof useVpn> }) {
  await waitFor(() => expect(result.current.catalog_loaded && result.current.status_loaded).toBe(true));
}

describe("VPN controller", () => {
  it("observes only while visible and never stops a VPN when the view unmounts", async () => {
    const { result, rerender, unmount } = renderHook(({ visible }) => useVpn(visible), { initialProps: { visible: false } });
    expect(loadVpnConnections).not.toHaveBeenCalled();
    expect(vpnStatus).not.toHaveBeenCalled();
    rerender({ visible: true });
    await ready(result);
    expect(connectVpn).not.toHaveBeenCalled();
    expect(stopVpn).not.toHaveBeenCalled();
    const calls = vi.mocked(vpnStatus).mock.calls.length;
    rerender({ visible: false });
    act(() => window.dispatchEvent(new Event("focus")));
    expect(vpnStatus).toHaveBeenCalledTimes(calls);
    unmount();
    expect(stopVpn).not.toHaveBeenCalled();
  });

  it("cancels a pending connection and ignores its late success after Stop", async () => {
    const pending = deferred<VpnStatus>();
    vi.mocked(connectVpn).mockReturnValueOnce(pending.promise);
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    let connect!: Promise<void>;
    act(() => { connect = result.current.connect(connection.connection_id); });
    expect(result.current.status.state).toBe("starting");
    await act(async () => { await result.current.stop(); });
    expect(stopVpn).toHaveBeenCalledOnce();
    expect(result.current.status.state).toBe("stopped");
    await act(async () => { pending.resolve(connected); await connect; });
    expect(result.current.status).toEqual(stopped);
    expect(result.current.action).toBeNull();
    expect(result.current.action_error).toBeNull();
  });

  it("coalesces simultaneous observations instead of starving a slow reply", async () => {
    const pending_status = deferred<VpnStatus>();
    const pending_catalog = deferred<VpnConnectionsSnapshot>();
    vi.mocked(vpnStatus).mockReturnValueOnce(pending_status.promise);
    vi.mocked(loadVpnConnections).mockReturnValueOnce(pending_catalog.promise);
    const { result } = renderHook(() => useVpn(true));
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refresh(); });
    expect(vpnStatus).toHaveBeenCalledOnce();
    expect(loadVpnConnections).toHaveBeenCalledOnce();
    await act(async () => {
      pending_status.resolve(stopped);
      pending_catalog.resolve(snapshot);
      await refresh;
    });
    expect(result.current.status_loaded && result.current.catalog_loaded).toBe(true);
    expect(result.current.status_loading || result.current.catalog_loading).toBe(false);
  });

  it("does not let a stale status refresh replace a successful connection", async () => {
    const old_status = deferred<VpnStatus>();
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    vi.mocked(vpnStatus).mockReturnValueOnce(old_status.promise).mockResolvedValue(connected);
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refresh(); });
    await act(async () => { await result.current.connect(connection.connection_id); });
    expect(result.current.status.state).toBe("connected");
    await act(async () => { old_status.resolve(stopped); await refresh; });
    expect(result.current.status).toEqual(connected);
  });

  it("keeps the editor's revision and fences a pre-save catalog response", async () => {
    const old_catalog = deferred<VpnConnectionsSnapshot>();
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    act(() => result.current.editConnection(connection));
    vi.mocked(loadVpnConnections).mockReturnValueOnce(old_catalog.promise);
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refresh(); });
    await act(async () => { expect(await result.current.saveConnection(input)).toBe(true); });
    expect(saveVpnConnection).toHaveBeenCalledWith("revision-1", input);
    expect(result.current.editor).toBeNull();
    await act(async () => { old_catalog.resolve(snapshot); await refresh; });
    expect(result.current.connections[0].name).toBe("Updated work");
    expect(result.current.catalog_loading).toBe(false);
  });

  it("marks failed status observations stale while retaining the last endpoint", async () => {
    vi.mocked(vpnStatus).mockResolvedValue(connected);
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    vi.mocked(vpnStatus).mockRejectedValueOnce(new Error("Broker unavailable"));
    await act(async () => { await result.current.refresh(); });
    expect(result.current.status).toEqual(connected);
    expect(result.current.status_stale).toBe(true);
    expect(result.current.status_error).toBe("Broker unavailable");
    await act(async () => { await result.current.connect(connection.connection_id); });
    expect(connectVpn).not.toHaveBeenCalled();
  });

  it("reports a connection error, then permits a fresh connection attempt", async () => {
    vi.mocked(connectVpn).mockRejectedValueOnce(new Error("Authentication failed"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    await act(async () => { await result.current.connect(connection.connection_id); });
    await waitFor(() => expect(result.current.status_stale).toBe(false));
    expect(result.current.action_error).toBe("Authentication failed");
    expect(result.current.status.state).toBe("stopped");
    vi.mocked(vpnStatus).mockResolvedValue(connected);
    await act(async () => { await result.current.connect(connection.connection_id); });
    expect(result.current.action_error).toBeNull();
    expect(result.current.status.state).toBe("connected");
  });

  it("clears a failed connection only after observing the same profile connected", async () => {
    vi.mocked(connectVpn).mockRejectedValueOnce(new Error("Authentication failed"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    await act(async () => { await result.current.connect(connection.connection_id); });
    await waitFor(() => expect(result.current.status_stale).toBe(false));
    expect(result.current.status.state).toBe("stopped");
    expect(result.current.action_error).toBe("Authentication failed");

    vi.mocked(vpnStatus).mockResolvedValue({ ...connected, state: "starting", running: false });
    await act(async () => { await result.current.refresh(); });
    expect(result.current.action_error).toBe("Authentication failed");

    vi.mocked(vpnStatus).mockResolvedValue({ ...connected, connection_id: "another-profile" });
    await act(async () => { await result.current.refresh(); });
    expect(result.current.action_error).toBe("Authentication failed");

    vi.mocked(vpnStatus).mockResolvedValue(connected);
    await act(async () => { await result.current.refresh(); });
    expect(result.current.action_error).toBeNull();
    expect(connectVpn).toHaveBeenCalledOnce();
  });

  it("does not clear a newer connection failure with a stale successful observation", async () => {
    vi.mocked(connectVpn)
      .mockRejectedValueOnce(new Error("First attempt failed"))
      .mockRejectedValueOnce(new Error("Latest attempt failed"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    await act(async () => { await result.current.connect(connection.connection_id); });
    await waitFor(() => expect(result.current.status_stale).toBe(false));
    const old_status = deferred<VpnStatus>();
    vi.mocked(vpnStatus).mockReturnValueOnce(old_status.promise);
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refresh(); });

    await act(async () => { await result.current.connect(connection.connection_id); });
    await waitFor(() => expect(result.current.status_stale).toBe(false));
    expect(result.current.action_error).toBe("Latest attempt failed");
    await act(async () => { old_status.resolve(connected); await refresh; });
    expect(result.current.action_error).toBe("Latest attempt failed");
    expect(result.current.status).toEqual(stopped);
  });

  it("blocks active-profile edit/delete and keeps disconnect errors distinct", async () => {
    vi.mocked(vpnStatus).mockResolvedValue(connected);
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    act(() => result.current.editConnection(connection));
    await act(async () => { await result.current.deleteConnection(connection.connection_id); });
    expect(result.current.editor).toBeNull();
    expect(deleteVpnConnection).not.toHaveBeenCalled();
    vi.mocked(stopVpn).mockRejectedValueOnce(new Error("Unable to stop container"));
    await act(async () => { await result.current.stop(); });
    expect(result.current.action_error).toBe("Unable to stop container");
    expect(result.current.status.state).toBe("connected");
  });

  it("clears a failed stop only after observing the VPN stopped", async () => {
    vi.mocked(vpnStatus).mockResolvedValue(connected);
    vi.mocked(stopVpn).mockRejectedValueOnce(new Error("Unable to stop container"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    await act(async () => { await result.current.stop(); });
    await waitFor(() => expect(result.current.status_stale).toBe(false));
    expect(result.current.action_error).toBe("Unable to stop container");

    vi.mocked(vpnStatus).mockResolvedValue({ ...connected, state: "stopping", running: false });
    await act(async () => { await result.current.refresh(); });
    expect(result.current.action_error).toBe("Unable to stop container");

    vi.mocked(vpnStatus).mockResolvedValue(stopped);
    await act(async () => { await result.current.refresh(); });
    expect(result.current.action_error).toBeNull();
    expect(stopVpn).toHaveBeenCalledOnce();
  });

  it("retains a failed editor without ever adding the submitted password to its state", async () => {
    vi.mocked(saveVpnConnection).mockRejectedValueOnce(new Error("Settings changed; reload before saving"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    act(() => result.current.editConnection(connection));
    await act(async () => { expect(await result.current.saveConnection({ ...input, password: "example-password" })).toBe(false); });
    expect(result.current.editor?.connection).toEqual(connection);
    expect(result.current.editor_error).toBe("Settings changed; reload before saving");
    expect(JSON.stringify(result.current.editor)).not.toContain("example-password");
    expect(result.current.connections).toEqual([connection]);
    expect(result.current.editor_saving).toBe(false);
  });
});
