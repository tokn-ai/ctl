// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sshReachability } from "../../lib/tauri";
import type { HostConnectionStatus, SshReachability, VpnStatus, WorkspaceHost } from "../../lib/types";
import { HOST_REACHABILITY_INTERVAL_MS, HOST_REACHABILITY_TIMEOUT_MS, useHostReachability } from "./useHostReachability";

vi.mock("../../lib/tauri", () => ({ sshReachability: vi.fn() }));

const host: WorkspaceHost = {
  host_id: "remote", name: "Development", preferred_method_id: "direct",
  connection_methods: [{ method_id: "direct", name: "Direct", target: { kind: "ssh", destination: "example.invalid" } }],
};
const disconnected: HostConnectionStatus = { state: "disconnected", method_names: [], message: null };
const available: SshReachability = { state: "available", reason: null, message: null };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function setup(overrides: Partial<Parameters<typeof useHostReachability>[0]> = {}) {
  const initial: Parameters<typeof useHostReachability>[0] = {
    ready: true, closing: false, hosts: [host], gateways: [],
    statuses: new Map([[host.host_id, disconnected]]), ...overrides,
  };
  return { ...renderHook(useHostReachability, { initialProps: initial }), initial };
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(sshReachability).mockResolvedValue(available);
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("SSH greeting observations", () => {
  it("waits for master observation and skips connected or actively connecting hosts", async () => {
    const { rerender, initial } = setup({ statuses: new Map() });
    expect(sshReachability).not.toHaveBeenCalled();
    await act(async () => { rerender({ ...initial, statuses: new Map([[host.host_id, {
      ...disconnected, state: "connected",
    }]]) }); });
    expect(sshReachability).not.toHaveBeenCalled();
    await act(async () => { rerender({ ...initial, statuses: new Map([[host.host_id, {
      ...disconnected, operation: { kind: "connect", state: "pending", method_name: "Direct", message: null },
    }]]) }); });
    expect(sshReachability).not.toHaveBeenCalled();
  });

  it("reports a greeting without changing master status or probing unknown runtime routes", async () => {
    const { result, initial } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "available", method_names: ["Direct"], checked_at_ms: expect.any(Number),
    }));
    expect(initial.statuses.get(host.host_id)).toBe(disconnected);
    expect(sshReachability).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ destination: "example.invalid" }));
  });

  it("skips missing projected routes without marking their endpoint unreachable", async () => {
    const unavailable = { ...host, connection_methods: [{ ...host.connection_methods[0],
      target: { ...host.connection_methods[0].target, unavailable: "The device is no longer available." },
    }] };
    const { result } = setup({ hosts: [unavailable] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("not_checked"));
    expect(sshReachability).not.toHaveBeenCalled();
  });

  it("retains positive evidence when another method cannot be checked", async () => {
    const multiple = { ...host, connection_methods: [...host.connection_methods, {
      method_id: "gateway", name: "Gateway", target: { kind: "ssh" as const, destination: "gateway.invalid" },
    }] };
    vi.mocked(sshReachability).mockImplementation(async (target) => target.kind === "ssh" && target.destination === "gateway.invalid"
      ? { state: "not_checked", reason: "route_requires_connection", message: "An SSH gateway would need a connection." } : available);
    const { result } = setup({ hosts: [multiple] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "available", method_names: ["Direct"], message: "Gateway: An SSH gateway would need a connection.",
    }));
  });

  it("throttles completed checks but refreshes on focus without duplicating in-flight checks", async () => {
    vi.useFakeTimers();
    const pending = deferred<SshReachability>();
    vi.mocked(sshReachability).mockReturnValueOnce(pending.promise);
    setup();
    await act(async () => { window.dispatchEvent(new Event("focus")); });
    expect(sshReachability).toHaveBeenCalledOnce();
    await act(async () => { pending.resolve(available); });
    await act(async () => { await vi.advanceTimersByTimeAsync(HOST_REACHABILITY_INTERVAL_MS - 1); });
    expect(sshReachability).toHaveBeenCalledOnce();
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(sshReachability).toHaveBeenCalledTimes(2);
    await act(async () => { window.dispatchEvent(new Event("focus")); });
    expect(sshReachability).toHaveBeenCalledTimes(3);
  });

  it("rejects an old route's successful result immediately when host settings change", async () => {
    const previous = deferred<SshReachability>();
    const current = deferred<SshReachability>();
    vi.mocked(sshReachability).mockReturnValueOnce(previous.promise).mockReturnValueOnce(current.promise);
    const { result, rerender, initial } = setup();
    const replacement = { ...host, connection_methods: [{ ...host.connection_methods[0],
      target: { kind: "ssh" as const, destination: "replacement.invalid" },
    }] };
    rerender({ ...initial, hosts: [replacement] });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("checking");
    await act(async () => { previous.resolve(available); });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("checking");
    await act(async () => { current.resolve({ state: "unavailable", reason: "connection_refused", message: null }); });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("unavailable");
  });

  it("invalidates VPN route changes while ignoring unrelated VPN status updates", async () => {
    const previous = deferred<SshReachability>();
    const current = deferred<SshReachability>();
    vi.mocked(sshReachability).mockReturnValueOnce(previous.promise).mockReturnValueOnce(current.promise);
    const routed = { ...host, connection_methods: [{ ...host.connection_methods[0],
      target: { ...host.connection_methods[0].target, vpn_connection_id: "route" },
    }] };
    const vpn: VpnStatus = { connection_id: "route", vpn_id: "runtime", state: "connected", running: true,
      endpoint: "socks5://127.0.0.1:10000", container_name: "container" };
    const { result, rerender, initial } = setup({ hosts: [routed], vpn_statuses: [vpn] });
    await act(async () => { rerender({ ...initial, vpn_statuses: [{ ...vpn, message: "Refreshed" }, {
      ...vpn, connection_id: "unrelated", vpn_id: "unrelated-runtime",
    }] }); });
    expect(sshReachability).toHaveBeenCalledOnce();
    rerender({ ...initial, vpn_statuses: [] });
    await act(async () => { previous.resolve(available); });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("checking");
    await act(async () => { current.resolve({ state: "not_checked", reason: "vpn_disconnected", message: "Connect the VPN to check SSH." }); });
    expect(result.current.statuses.get(host.host_id)).toMatchObject({ state: "not_checked", reason: "vpn_disconnected" });
  });

  it("bounds stalled IPC and ignores its late success", async () => {
    vi.useFakeTimers();
    const pending = deferred<SshReachability>();
    vi.mocked(sshReachability).mockReturnValue(pending.promise);
    const { result } = setup();
    await act(async () => { await vi.advanceTimersByTimeAsync(HOST_REACHABILITY_TIMEOUT_MS); });
    expect(result.current.statuses.get(host.host_id)).toMatchObject({ state: "unknown", reason: "check_failed" });
    await act(async () => { pending.resolve(available); });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("unknown");
    expect(vi.getTimerCount()).toBe(1);
  });

  it("does not use local VPN state for a VPN running on a jump host", async () => {
    const gateway = { gateway_id: "jump", name: "Jump", destination: "jump.example" };
    const routed: WorkspaceHost = { ...host, connection_methods: [{ ...host.connection_methods[0], target: {
      ...host.connection_methods[0].target, gateway_route: [
        { gateway_id: gateway.gateway_id, mode: "automatic" }, { vpn_connection_id: "office" },
      ],
    } }] };
    const vpn: VpnStatus = { connection_id: "office", state: "connected", running: true, endpoint: null, container_name: null };
    const { initial, rerender, result } = setup({ hosts: [routed], gateways: [gateway], vpn_statuses: [vpn] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("available"));
    await act(async () => { rerender({ ...initial, vpn_statuses: [], vpn_status_stale: true }); });
    expect(sshReachability).toHaveBeenCalledOnce();
    expect(result.current.statuses.get(host.host_id)?.state).toBe("available");
  });

  it("limits concurrency across methods and skips queued work for removed hosts", async () => {
    const pending = Array.from({ length: 4 }, () => deferred<SshReachability>());
    pending.forEach((item) => { vi.mocked(sshReachability).mockReturnValueOnce(item.promise); });
    const hosts = Array.from({ length: 6 }, (_, index) => ({ ...host, host_id: `host-${index}` }));
    const { result, rerender, initial } = setup({ hosts, statuses: new Map(hosts.map((item) => [item.host_id, disconnected])) });
    expect(sshReachability).toHaveBeenCalledTimes(4);
    await act(async () => { rerender({ ...initial, hosts: hosts.slice(0, 5) }); });
    await act(async () => { pending.forEach((item) => item.resolve(available)); });
    expect(sshReachability).toHaveBeenCalledTimes(5);
    expect(result.current.statuses.get("host-4")?.state).toBe("available");
  });
});

it("resolves linked hop settings from the current host context and invalidates their stale observation", async () => {
  const routed = { ...host, connection_methods: [{ ...host.connection_methods[0], target: { ...host.connection_methods[0].target,
    gateway_route: [{ host_id: "jump", method_id: "ssh", mode: "automatic" as const }],
  } }] };
  const jump: WorkspaceHost = { host_id: "jump", name: "Jump", preferred_method_id: "ssh",
    connection_methods: [{ method_id: "ssh", name: "SSH", target: { kind: "ssh", destination: "old.jump" } }] };
  const previous = deferred<SshReachability>();
  const current = deferred<SshReachability>();
  vi.mocked(sshReachability).mockReturnValueOnce(previous.promise).mockReturnValueOnce(current.promise);
  const { result, rerender, initial } = setup({ hosts: [routed, jump] });
  expect(sshReachability).toHaveBeenCalledWith(expect.objectContaining({ destination: host.connection_methods[0].target.destination,
    gateways: [expect.objectContaining({ destination: "old.jump" })] }));
  const changed = { ...jump, connection_methods: [{ ...jump.connection_methods[0], target: { kind: "ssh" as const, destination: "new.jump" } }] };
  rerender({ ...initial, hosts: [routed, changed] });
  expect(sshReachability).toHaveBeenCalledWith(expect.objectContaining({ gateways: [expect.objectContaining({ destination: "new.jump" })] }));
  await act(async () => { previous.resolve(available); });
  expect(result.current.statuses.get(host.host_id)?.state).toBe("checking");
  await act(async () => { current.resolve({ state: "not_checked", reason: "route_requires_connection", message: "The hop requires a connection." }); });
  expect(result.current.statuses.get(host.host_id)?.state).toBe("not_checked");
});
