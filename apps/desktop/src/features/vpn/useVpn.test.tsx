// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { VpnConnection, VpnConnectionInput, VpnConnectionsSnapshot, VpnSnapshot, VpnStatus } from "../../lib/types";
import { connectVpn, deleteVpnConnection, loadVpnConnections, openVpnSignIn, saveVpnConnection, stopVpn, vpnStatus } from "../../lib/tauri";
import { useVpn, VPN_STATUS_INTERVAL_MS } from "./useVpn";

vi.mock("../../lib/tauri", () => ({
  connectVpn: vi.fn(), deleteVpnConnection: vi.fn(), loadVpnConnections: vi.fn(),
  openVpnSignIn: vi.fn(), saveVpnConnection: vi.fn(), stopVpn: vi.fn(), vpnStatus: vi.fn(),
}));

const connection: VpnConnection = {
  connection_id: "work", name: "Work", url: "https://vpn.example.test", username: "example-user",
  has_password: true, auth_method: null, target_ip: null,
};
const research = { ...connection, connection_id: "research", name: "Research" };
const catalog: VpnConnectionsSnapshot = { revision: "revision-1", connections: [connection, research] };
const input: VpnConnectionInput = {
  connection_id: "work", name: "Updated work", url: connection.url, username: connection.username,
  password: null, auth_method: null, target_ip: null,
};
const stopped: VpnStatus = { state: "stopped", running: false, connection_id: null, endpoint: null, container_name: null };
let backend: VpnSnapshot;

function runtime(vpn_id = "work", overrides: Partial<VpnStatus> = {}): VpnStatus {
  return {
    vpn_id, connection_id: vpn_id, state: "connected", running: true,
    vpn_url: "https://vpn.example.test", username: "example-user",
    endpoint: `socks5h://127.0.0.1:${vpn_id === "work" ? "49152" : "49153"}`, container_name: `sample-${vpn_id}`,
    ...overrides,
  };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}
function observe(...connections: VpnStatus[]): VpnSnapshot {
  return { connections, supports_multiple: true };
}
function status(result: { current: ReturnType<typeof useVpn> }, vpn_id: string) {
  return result.current.statuses.find((item) => item.vpn_id === vpn_id);
}
async function ready(result: { current: ReturnType<typeof useVpn> }) {
  await waitFor(() => expect(result.current.catalog_loaded && result.current.status_loaded).toBe(true));
}

beforeEach(() => {
  vi.resetAllMocks();
  backend = observe();
  vi.mocked(loadVpnConnections).mockResolvedValue(catalog);
  vi.mocked(vpnStatus).mockImplementation(async () => ({ ...backend, connections: [...backend.connections] }));
  vi.mocked(connectVpn).mockImplementation(async (vpn_id) => {
    const next = runtime(vpn_id);
    backend.connections = [...backend.connections.filter((item) => item.vpn_id !== vpn_id), next];
    return next;
  });
  vi.mocked(stopVpn).mockImplementation(async (vpn_id) => {
    backend.connections = backend.connections.filter((item) => item.vpn_id !== vpn_id);
    return { ...stopped, vpn_id };
  });
  vi.mocked(saveVpnConnection).mockResolvedValue({ revision: "revision-2", connections: [{ ...connection, name: input.name }, research] });
  vi.mocked(deleteVpnConnection).mockResolvedValue({ revision: "revision-2", connections: [research] });
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("VPN controller", () => {
  it("observes only while enabled and never owns a VPN's lifetime", async () => {
    const { result, rerender, unmount } = renderHook(({ enabled }) => useVpn(enabled), { initialProps: { enabled: false } });
    expect(vpnStatus).not.toHaveBeenCalled();
    rerender({ enabled: true });
    await ready(result);
    expect(connectVpn).not.toHaveBeenCalled();
    const calls = vi.mocked(vpnStatus).mock.calls.length;
    rerender({ enabled: false });
    act(() => window.dispatchEvent(new Event("focus")));
    expect(vpnStatus).toHaveBeenCalledTimes(calls);
    unmount();
    expect(stopVpn).not.toHaveBeenCalled();
  });

  it("discovers multiple external VPNs without mounting the VPN panel", async () => {
    vi.useFakeTimers();
    const { result, unmount } = renderHook(() => useVpn(true));
    await act(async () => {});
    backend = observe(runtime("cli-a", { connection_id: null }), runtime("cli-b", { connection_id: null }));
    await act(async () => { await vi.advanceTimersByTimeAsync(VPN_STATUS_INTERVAL_MS); });
    expect(result.current.statuses).toEqual(backend.connections);
    expect(loadVpnConnections).toHaveBeenCalledOnce();
    backend = observe();
    await act(async () => { await vi.advanceTimersByTimeAsync(VPN_STATUS_INTERVAL_MS); });
    expect(result.current.statuses).toEqual([]);
    unmount();
    expect(connectVpn).not.toHaveBeenCalled();
    expect(stopVpn).not.toHaveBeenCalled();
  });

  it("observes and targets a CLI VPN despite a catalog failure", async () => {
    vi.mocked(loadVpnConnections).mockRejectedValue(new Error("Catalog unavailable"));
    backend = observe(runtime("cli", { connection_id: null }));
    const { result } = renderHook(() => useVpn(true));
    await waitFor(() => expect(result.current.status_loaded).toBe(true));
    expect(result.current.catalog_loaded).toBe(false);
    await act(async () => { await result.current.stop("cli"); });
    expect(stopVpn).toHaveBeenCalledWith("cli");
    expect(result.current.statuses).toEqual([]);
  });

  it("connects two profiles concurrently and isolates their results and errors", async () => {
    const work = deferred<VpnStatus>();
    const other = deferred<VpnStatus>();
    vi.mocked(connectVpn).mockImplementation((id) => id === "work" ? work.promise : other.promise);
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => { first = result.current.connect("work"); second = result.current.connect("research"); });
    expect(result.current.actions.size).toBe(2);
    expect(result.current.statuses.every((item) => item.state === "starting")).toBe(true);
    backend = observe(runtime("research"));
    await act(async () => { other.resolve(runtime("research")); await second; });
    expect(status(result, "research")?.state).toBe("connected");
    expect(status(result, "work")?.state).toBe("starting");
    await act(async () => { work.reject(new Error("Work authentication failed")); await first; });
    expect(result.current.action_errors.get("work")).toBe("Work authentication failed");
    expect(result.current.action_errors.has("research")).toBe(false);
    expect(result.current.statuses).toEqual([runtime("research")]);
    expect(stopVpn).not.toHaveBeenCalled();
  });

  it("stops only the selected VPN while the other remains connected", async () => {
    backend = observe(runtime(), runtime("research"));
    const stop = deferred<VpnStatus>();
    vi.mocked(stopVpn).mockReturnValueOnce(stop.promise);
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    let pending!: Promise<void>;
    act(() => { pending = result.current.stop("work"); });
    expect(status(result, "work")?.state).toBe("stopping");
    expect(status(result, "research")?.state).toBe("connected");
    backend = observe(runtime("research"));
    await act(async () => { stop.resolve({ ...stopped, vpn_id: "work" }); await pending; });
    expect(stopVpn).toHaveBeenCalledExactlyOnceWith("work");
    expect(result.current.statuses).toEqual([runtime("research")]);
  });

  it("cancels one pending connection and ignores its late success without canceling another", async () => {
    const pending = deferred<VpnStatus>();
    vi.mocked(connectVpn).mockImplementation((id) => id === "work" ? pending.promise : Promise.resolve(runtime(id)));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    let first!: Promise<void>;
    act(() => { first = result.current.connect("work"); });
    backend = observe(runtime("research"));
    await act(async () => { await result.current.connect("research"); await result.current.stop("work"); });
    await act(async () => { pending.resolve(runtime()); await first; });
    expect(result.current.statuses).toEqual([runtime("research")]);
    expect(result.current.actions.size).toBe(0);
    expect(stopVpn).toHaveBeenCalledExactlyOnceWith("work");
  });

  it("merges polling changes for other VPNs while one is starting", async () => {
    const pending = deferred<VpnStatus>();
    vi.mocked(connectVpn).mockReturnValueOnce(pending.promise);
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    act(() => { void result.current.connect("work"); });
    backend = observe(runtime("external", { connection_id: null }));
    await act(async () => { await result.current.refresh(); });
    expect(status(result, "work")?.state).toBe("starting");
    expect(status(result, "external")?.state).toBe("connected");
  });

  it("starts a fresh reconciliation after completion and rejects an older in-flight snapshot", async () => {
    const old = deferred<VpnSnapshot>();
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    vi.mocked(vpnStatus).mockReturnValueOnce(old.promise);
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refresh(); });
    const calls = vi.mocked(vpnStatus).mock.calls.length;
    await act(async () => { await result.current.connect("work"); });
    expect(vi.mocked(vpnStatus).mock.calls.length).toBeGreaterThan(calls);
    await act(async () => { old.resolve(observe()); await refresh; });
    expect(status(result, "work")?.state).toBe("connected");
  });

  it("keeps a failed start uncertain until observed and does not freeze another profile", async () => {
    vi.mocked(connectVpn).mockRejectedValueOnce(new Error("Request timed out"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    const reconcile = deferred<VpnSnapshot>();
    vi.mocked(vpnStatus).mockReturnValueOnce(reconcile.promise);
    await act(async () => { await result.current.connect("work"); });
    expect(result.current.uncertain_ids.has("work")).toBe(true);
    await act(async () => { await result.current.connect("work"); });
    act(() => result.current.editConnection(connection));
    await act(async () => { await result.current.deleteConnection("work"); });
    expect(connectVpn).toHaveBeenCalledOnce();
    expect(result.current.editor).toBeNull();
    expect(deleteVpnConnection).not.toHaveBeenCalled();
    const other = deferred<VpnStatus>();
    vi.mocked(connectVpn).mockReturnValueOnce(other.promise);
    act(() => { void result.current.connect("research"); });
    expect(connectVpn).toHaveBeenCalledTimes(2);
    expect(status(result, "research")?.state).toBe("starting");
    await act(async () => { reconcile.resolve(observe(runtime())); });
    expect(result.current.uncertain_ids.has("work")).toBe(false);
    expect(result.current.action_errors.has("work")).toBe(false);
    expect(status(result, "work")?.state).toBe("connected");
    expect(status(result, "research")?.state).toBe("starting");
  });

  it("coalesces simultaneous observations", async () => {
    const pending = deferred<VpnSnapshot>();
    vi.mocked(vpnStatus).mockReturnValueOnce(pending.promise);
    const { result } = renderHook(() => useVpn(true));
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refresh(); });
    expect(vpnStatus).toHaveBeenCalledOnce();
    await act(async () => { pending.resolve(observe()); await refresh; });
    expect(result.current.status_loaded).toBe(true);
    expect(result.current.status_loading).toBe(false);
  });

  it("keeps existing VPNs visible when polling fails and permits a targeted stop", async () => {
    backend = observe(runtime(), runtime("research"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    vi.mocked(vpnStatus).mockRejectedValueOnce(new Error("Broker unavailable"));
    await act(async () => { await result.current.refresh(); });
    expect(result.current.statuses).toHaveLength(2);
    expect(result.current.status_stale).toBe(true);
    await act(async () => { await result.current.stop("work"); });
    expect(result.current.statuses).toEqual([runtime("research")]);
  });

  it("preserves an older daemon's active VPN and blocks only unsupported additional connects", async () => {
    backend = { supports_multiple: false, connections: [runtime("legacy", { connection_id: null })] };
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    await act(async () => { await result.current.connect("work"); });
    expect(connectVpn).not.toHaveBeenCalled();
    expect(result.current.supports_multiple).toBe(false);
    await act(async () => { await result.current.stop("legacy"); });
    expect(stopVpn).not.toHaveBeenCalled();
    expect(result.current.statuses).toHaveLength(1);
    backend.connections = [];
    await act(async () => { await result.current.refresh(); });
    await act(async () => { await result.current.connect("work"); });
    expect(connectVpn).toHaveBeenCalledWith("work");
  });

  it("clears failed stop errors only when that runtime disappears", async () => {
    backend = observe(runtime(), runtime("research"));
    vi.mocked(stopVpn).mockRejectedValueOnce(new Error("Unable to stop work"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    await act(async () => { await result.current.stop("work"); });
    expect(result.current.action_errors.get("work")).toBe("Unable to stop work");
    backend = observe(runtime());
    await act(async () => { await result.current.refresh(); });
    expect(result.current.action_errors.has("work")).toBe(true);
    backend = observe();
    await act(async () => { await result.current.refresh(); });
    expect(result.current.action_errors.has("work")).toBe(false);
  });

  it("blocks active-profile edits and deletes while allowing changes to another profile", async () => {
    backend = observe(runtime());
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    act(() => result.current.editConnection(connection));
    await act(async () => { await result.current.deleteConnection("work"); });
    expect(result.current.editor).toBeNull();
    expect(deleteVpnConnection).not.toHaveBeenCalled();
    act(() => result.current.editConnection(research));
    expect(result.current.editor?.connection).toEqual(research);
  });

  it("keeps the editor revision and fences a pre-save catalog response", async () => {
    const old = deferred<VpnConnectionsSnapshot>();
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    act(() => result.current.editConnection(connection));
    vi.mocked(loadVpnConnections).mockReturnValueOnce(old.promise);
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refresh(); });
    await act(async () => { expect(await result.current.saveConnection(input)).toBe(true); });
    expect(saveVpnConnection).toHaveBeenCalledWith("revision-1", input);
    await act(async () => { old.resolve(catalog); await refresh; });
    expect(result.current.connections[0].name).toBe("Updated work");
    expect(result.current.editor).toBeNull();
  });

  it("retains a failed editor without keeping its submitted password", async () => {
    vi.mocked(saveVpnConnection).mockRejectedValueOnce(new Error("Settings changed"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    act(() => result.current.editConnection(connection));
    await act(async () => { expect(await result.current.saveConnection({ ...input, password: "sample-password" })).toBe(false); });
    expect(result.current.editor?.connection).toEqual(connection);
    expect(JSON.stringify(result.current.editor)).not.toContain("sample-password");
    expect(result.current.editor_error).toBe("Settings changed");
  });
});


describe("Tailscale VPN controller", () => {
  const tailscale: VpnConnection = { provider: "tailscale", connection_id: "tailnet", name: "Tailnet", hostname: null, accept_routes: false };
  const pending = runtime("tailnet", { provider: "tailscale", state: "starting", running: false, endpoint: null,
    auth_url: "https://login.tailscale.com/a/example" });

  it("treats missing provider capabilities as OpenConnect-only", async () => {
    vi.mocked(loadVpnConnections).mockResolvedValue({ revision: "1", connections: [tailscale] });
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    expect(result.current.supported_providers).toEqual(["openconnect"]);
    await act(async () => { await result.current.connect("tailnet"); });
    expect(connectVpn).not.toHaveBeenCalled();
  });

  it("returns from connect while browser sign-in is pending, observes completion, and retains targeted stop", async () => {
    vi.mocked(loadVpnConnections).mockResolvedValue({ revision: "1", connections: [tailscale] });
    backend.supported_providers = ["openconnect", "tailscale"];
    vi.mocked(connectVpn).mockImplementation(async () => { backend.connections = [pending]; return pending; });
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    await act(async () => { await result.current.connect("tailnet"); });
    expect(result.current.actions.size).toBe(0);
    expect(status(result, "tailnet")?.auth_url).toBe(pending.auth_url);
    await act(async () => { await result.current.signIn("tailnet"); });
    expect(openVpnSignIn).toHaveBeenCalledExactlyOnceWith("tailnet");
    backend.connections = [{ ...pending, state: "connected", running: true, auth_url: null }];
    await act(async () => { await result.current.refresh(); });
    expect(status(result, "tailnet")?.state).toBe("connected");
    await act(async () => { await result.current.stop("tailnet"); });
    expect(stopVpn).toHaveBeenCalledWith("tailnet");
    expect(result.current.statuses).toEqual([]);
  });

  it("does not undo a stop or block cancellation while browser opening is pending", async () => {
    backend = { ...observe(pending), supported_providers: ["openconnect", "tailscale"] };
    const opening = deferred<void>();
    vi.mocked(openVpnSignIn).mockReturnValue(opening.promise);
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    let sign_in!: Promise<void>;
    act(() => { sign_in = result.current.signIn("tailnet"); });
    expect(result.current.signing_in_ids.has("tailnet")).toBe(true);
    await act(async () => { await result.current.stop("tailnet"); });
    await act(async () => { opening.reject(new Error("Browser unavailable")); await sign_in; });
    expect(result.current.statuses).toEqual([]);
    expect(result.current.action_errors.has("tailnet")).toBe(false);
    expect(result.current.signing_in_ids.size).toBe(0);
  });

  it("reports sign-in failure without treating runtime state as uncertain", async () => {
    backend = observe(pending);
    vi.mocked(openVpnSignIn).mockRejectedValue(new Error("Browser unavailable"));
    const { result } = renderHook(() => useVpn(true));
    await ready(result);
    await act(async () => { await result.current.signIn("tailnet"); });
    expect(result.current.action_errors.get("tailnet")).toBe("Browser unavailable");
    expect(result.current.uncertain_ids.size).toBe(0);
    expect(status(result, "tailnet")).toEqual(pending);
    backend.connections = [{ ...pending, state: "connected", auth_url: null }];
    await act(async () => { await result.current.refresh(); });
    expect(result.current.action_errors.has("tailnet")).toBe(false);
  });
});
