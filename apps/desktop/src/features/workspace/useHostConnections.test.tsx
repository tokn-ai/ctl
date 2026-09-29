// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { disconnectSshHost, probeSshHost, sshConnectionStatus, sshReachability } from "../../lib/tauri";
import type { SshConnectionStatus, SshConnectionTarget, WorkspaceHost } from "../../lib/types";
import { HOST_STATUS_INTERVAL_MS, HOST_STATUS_TIMEOUT_MS, useHostConnections } from "./useHostConnections";

vi.mock("../../lib/tauri", () => ({
  disconnectSshHost: vi.fn(),
  probeSshHost: vi.fn(),
  sshConnectionStatus: vi.fn(),
  sshReachability: vi.fn(),
}));

const target: SshConnectionTarget = {
  kind: "ssh", host_id: "remote", destination: "direct", method_id: "direct",
};
const host: WorkspaceHost = {
  host_id: "remote",
  name: "Development",
  source: "saved",
  preferred_method_id: "direct",
  connection_methods: [{
    method_id: "direct", name: "Direct", target: { kind: "ssh", destination: "direct" },
  }],
};
const disconnected: SshConnectionStatus = { connected: false, manually_disconnected: false };
const connected: SshConnectionStatus = { connected: true, manually_disconnected: false };
const manuallyDisconnected: SshConnectionStatus = { connected: false, manually_disconnected: true };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function setup(overrides: Partial<Parameters<typeof useHostConnections>[0]> = {}) {
  const initial: Parameters<typeof useHostConnections>[0] = {
    ready: true,
    closing: false,
    hosts: [host],
    targets: [target],
    gateways: [],
    onPause: vi.fn(async () => undefined),
    onResume: vi.fn(),
    ...overrides,
  };
  return { ...renderHook(useHostConnections, { initialProps: initial }), initial };
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(sshConnectionStatus).mockResolvedValue(disconnected);
  vi.mocked(sshReachability).mockResolvedValue({ state: "unavailable", reason: "connection_refused", message: "SSH port refused the connection." });
  vi.mocked(disconnectSshHost).mockResolvedValue(undefined);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("live host connections", () => {
  it("keeps a manually disconnected host paused even when its SSH greeting is available", async () => {
    vi.mocked(sshConnectionStatus).mockResolvedValue(manuallyDisconnected);
    vi.mocked(sshReachability).mockResolvedValue({ state: "available", reason: null, message: null });
    const previous = { ...target, destination: "previous.invalid" };
    const { result, initial } = setup({ targets: [target, previous] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.reachability?.state).toBe("available"));
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "disconnected", method_names: [], manually_disconnected: true,
      observation: { availability: "unavailable" }, reachability: { method_names: ["Direct"] },
    });
    expect(result.current.isPaused(target)).toBe(true);
    expect(initial.onResume).not.toHaveBeenCalled();
    expect(sshReachability).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ destination: "direct" }));
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it("observes startup, periodic, and focus changes without authenticating", async () => {
    vi.useFakeTimers();
    const { result, rerender, initial } = setup({ ready: false });
    expect(sshConnectionStatus).not.toHaveBeenCalled();

    await act(async () => { rerender({ ...initial, ready: true }); });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected");
    vi.mocked(sshConnectionStatus).mockResolvedValue(connected);
    await act(async () => { await vi.advanceTimersByTimeAsync(HOST_STATUS_INTERVAL_MS); });
    expect(result.current.statuses.get(host.host_id)).toMatchObject({ state: "connected", method_names: ["Direct"] });

    vi.mocked(sshConnectionStatus).mockResolvedValue(disconnected);
    await act(async () => { window.dispatchEvent(new Event("focus")); });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected");
    expect(probeSshHost).not.toHaveBeenCalled();
    expect(disconnectSshHost).not.toHaveBeenCalled();
    expect(initial.onPause).not.toHaveBeenCalled();
    expect(initial.onResume).not.toHaveBeenCalled();
  });

  it("aggregates connected methods and reports observation failures", async () => {
    const methods: WorkspaceHost = {
      ...host,
      connection_methods: [
        ...host.connection_methods,
        { method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "vpn" } },
      ],
    };
    vi.mocked(sshConnectionStatus).mockImplementation(async (method) =>
      method.kind === "ssh" && method.destination === "vpn" ? connected : disconnected);
    const { result } = setup({ hosts: [methods] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "connected", method_names: ["VPN"], message: null,
    }));

    vi.mocked(sshConnectionStatus).mockResolvedValue(connected);
    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)?.method_names).toEqual(["Direct", "VPN"]);

    vi.mocked(sshConnectionStatus).mockRejectedValue(new Error("Broker unavailable"));
    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "error", method_names: [], message: "Broker unavailable",
    });
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it("quiesces the host before disconnecting every saved method and distinct runtime route", async () => {
    const pause = deferred<void>();
    const onPause = vi.fn(() => pause.promise);
    const methods: WorkspaceHost = {
      ...host,
      connection_methods: [
        ...host.connection_methods,
        { method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "vpn" } },
      ],
    };
    const saved = structuredClone(methods);
    const { result } = setup({
      hosts: [methods],
      targets: [target, { ...target, destination: "old-route" }],
      onPause,
    });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected"));
    let disconnect!: Promise<void>;
    act(() => { disconnect = result.current.disconnect(target); });
    expect(onPause).toHaveBeenCalledExactlyOnceWith(host.host_id);
    expect(result.current.isPaused(target)).toBe(true);
    expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnecting");
    expect(disconnectSshHost).not.toHaveBeenCalled();

    await act(async () => {
      pause.resolve();
      await disconnect;
    });
    expect(disconnectSshHost).toHaveBeenCalledOnce();
    expect(vi.mocked(disconnectSshHost).mock.calls[0][0].map((method) =>
      method.kind === "ssh" ? method.destination : "local")).toEqual(["direct", "vpn", "old-route"]);
    expect(methods).toEqual(saved);
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "disconnected", method_names: [], message: expect.stringContaining("Disconnected manually"),
      manually_disconnected: true,
    });
  });

  it("does not let an older connected observation overwrite a manual disconnect", async () => {
    const observation = deferred<SshConnectionStatus>();
    const disconnecting = deferred<void>();
    vi.mocked(sshConnectionStatus).mockReturnValueOnce(observation.promise);
    vi.mocked(disconnectSshHost).mockReturnValueOnce(disconnecting.promise);
    const { result } = setup();
    let disconnect!: Promise<void>;
    act(() => { disconnect = result.current.disconnect(target); });
    await act(async () => { observation.resolve(connected); });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnecting");
    expect(result.current.isPaused(target)).toBe(true);

    await act(async () => {
      disconnecting.resolve();
      await disconnect;
    });
    expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected");
  });

  it("disconnects the previous master mode after a saved method is switched", async () => {
    const previous: SshConnectionTarget = { ...target, ssh_config_alias: "direct" };
    const updated: WorkspaceHost = {
      ...host,
      connection_methods: [{
        ...host.connection_methods[0], ssh_config_alias: "direct", use_ssh_config_master: false,
      }],
    };
    vi.mocked(sshConnectionStatus).mockImplementation(async (candidate) =>
      candidate.kind === "ssh" && candidate.use_ssh_config_master === false ? disconnected : connected);
    const { result } = setup({ hosts: [updated], targets: [previous] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "connected", method_names: ["Previous connection"],
    }));
    await act(async () => result.current.disconnect(previous));
    expect(disconnectSshHost).toHaveBeenCalledExactlyOnceWith([
      expect.objectContaining({ ssh_config_alias: "direct", use_ssh_config_master: false }),
      previous,
    ]);
  });

  it.each(["connected", "error"] as const)("ignores stale %s observations after connection settings change", async (outcome) => {
    const observation = deferred<SshConnectionStatus>();
    vi.mocked(sshConnectionStatus).mockReturnValueOnce(observation.promise);
    const { result, rerender, initial } = setup();
    const updated: WorkspaceHost = {
      ...host,
      connection_methods: [{
        method_id: "direct", name: "Updated route", target: { kind: "ssh", destination: "replacement" },
      }],
    };
    rerender({ ...initial, hosts: [updated], targets: [{ ...target, destination: "replacement" }] });
    await act(async () => {
      if (outcome === "connected") observation.resolve(connected);
      else observation.reject(new Error("Old route failed"));
    });
    expect(result.current.statuses.get(host.host_id)?.state).not.toBe("connected");
    expect(result.current.statuses.get(host.host_id)?.message).not.toBe("Old route failed");

    await act(async () => result.current.refresh());
    expect(sshConnectionStatus).toHaveBeenLastCalledWith(expect.objectContaining({ destination: "replacement" }));
    expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected");
  });

  it("reports failed disconnects without claiming the master has closed", async () => {
    vi.mocked(sshConnectionStatus).mockResolvedValue(connected);
    vi.mocked(disconnectSshHost).mockRejectedValue(new Error("Could not close SSH master"));
    const { result, initial } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("connected"));
    await act(async () => {
      await expect(result.current.disconnect(target)).rejects.toThrow("Could not close SSH master");
    });
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "error", message: "Could not close SSH master",
    });
    expect(initial.onPause).toHaveBeenCalledExactlyOnceWith(host.host_id);
    expect(initial.onResume).not.toHaveBeenCalled();
    expect(result.current.isPaused(target)).toBe(true);

    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "error", method_names: ["Direct"], message: "Could not close SSH master",
    });
    expect(result.current.isPaused(target)).toBe(true);
    expect(initial.onResume).not.toHaveBeenCalled();

    await act(async () => result.current.connectionChanged(target, "connected"));
    expect(result.current.statuses.get(host.host_id)).toMatchObject({ state: "connected", message: null });
    expect(result.current.isPaused(target)).toBe(false);
    expect(initial.onResume).toHaveBeenCalledExactlyOnceWith(host.host_id);
  });

  it.each(["success", "error"] as const)("ignores a stale cross-window pause %s after settings change", async (outcome) => {
    const pause = deferred<void>();
    const onPause = vi.fn(() => pause.promise);
    vi.mocked(sshConnectionStatus).mockResolvedValueOnce(manuallyDisconnected);
    const { result, rerender, initial } = setup({ onPause });
    await waitFor(() => expect(onPause).toHaveBeenCalledOnce());
    const updated: WorkspaceHost = {
      ...host,
      connection_methods: [{
        method_id: "direct", name: "Updated route", target: { kind: "ssh", destination: "replacement" },
      }],
    };
    rerender({ ...initial, hosts: [updated], targets: [{ ...target, destination: "replacement" }] });
    await act(async () => {
      if (outcome === "success") pause.resolve();
      else pause.reject(new Error("Old host pause failed"));
    });
    expect(result.current.statuses.get(host.host_id)?.message).not.toBe("Old host pause failed");
    expect(result.current.statuses.get(host.host_id)?.message ?? "").not.toContain("Disconnected manually");

    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected");
  });

  it("honors a disconnect from another window and resumes only after a connected observation", async () => {
    vi.mocked(sshConnectionStatus).mockResolvedValue(manuallyDisconnected);
    const { result, initial } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected"));
    expect(initial.onPause).toHaveBeenCalledExactlyOnceWith(host.host_id);
    expect(result.current.isPaused(target)).toBe(true);
    await act(async () => result.current.refresh());
    expect(initial.onPause).toHaveBeenCalledOnce();
    expect(initial.onResume).not.toHaveBeenCalled();
    expect(disconnectSshHost).not.toHaveBeenCalled();

    vi.mocked(sshConnectionStatus).mockResolvedValue(connected);
    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)?.state).toBe("connected");
    expect(initial.onResume).toHaveBeenCalledExactlyOnceWith(host.host_id);
    expect(result.current.isPaused(target)).toBe(false);
  });

  it("resumes a host when another window reconnects one method while other methods stay paused", async () => {
    const methods: WorkspaceHost = {
      ...host,
      connection_methods: [
        ...host.connection_methods,
        { method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "vpn" } },
      ],
    };
    vi.mocked(sshConnectionStatus).mockResolvedValue(manuallyDisconnected);
    const { result, initial } = setup({ hosts: [methods] });
    await waitFor(() => expect(initial.onPause).toHaveBeenCalledOnce());
    expect(result.current.isPaused(target)).toBe(true);

    vi.mocked(sshConnectionStatus).mockImplementation(async (method) =>
      method.kind === "ssh" && method.destination === "vpn" ? manuallyDisconnected : connected);
    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)?.method_names).toEqual(["Direct"]);
    expect(result.current.statuses.get(host.host_id)?.message).toBeNull();
    expect(result.current.isPaused(target)).toBe(false);
    expect(initial.onResume).toHaveBeenCalledExactlyOnceWith(host.host_id);
  });

  it("pauses a live master left behind by a failed disconnect in another window", async () => {
    vi.mocked(sshConnectionStatus).mockResolvedValue({ connected: true, manually_disconnected: true });
    const { result, initial } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "error", method_names: ["Direct"], message: expect.stringContaining("still open"),
      manually_disconnected: true,
      observation: { availability: "available" },
    }));
    expect(initial.onPause).toHaveBeenCalledExactlyOnceWith(host.host_id);
    expect(result.current.isPaused(target)).toBe(true);
    expect(initial.onResume).not.toHaveBeenCalled();
  });

  it("keeps a paused host paused through authentication and resumes on explicit success", async () => {
    const { result, initial } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected"));
    await act(async () => result.current.disconnect(target));
    act(() => result.current.connectionChanged(target, "connecting"));
    expect(result.current.statuses.get(host.host_id)?.state).toBe("connecting");
    expect(result.current.isPaused(target)).toBe(true);
    expect(initial.onResume).not.toHaveBeenCalled();

    vi.mocked(sshConnectionStatus).mockResolvedValue(connected);
    await act(async () => result.current.connectionChanged(target, "connected"));
    expect(initial.onResume).toHaveBeenCalledExactlyOnceWith(host.host_id);
    expect(result.current.isPaused(target)).toBe(false);
    expect(result.current.statuses.get(host.host_id)?.state).toBe("connected");
    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)?.manually_disconnected).toBe(false);
    expect(initial.onResume).toHaveBeenCalledOnce();
  });
});

describe("separate SSH observations and connection attempts", () => {
  const multipleMethods: WorkspaceHost = {
    ...host,
    connection_methods: [
      ...host.connection_methods,
      { method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "vpn" } },
    ],
  };
  const alternate: SshConnectionTarget = { ...target, destination: "vpn", method_id: "vpn" };

  it("keeps an available master visible when another method is connecting or fails", async () => {
    vi.mocked(sshConnectionStatus).mockImplementation(async (candidate) =>
      candidate.kind === "ssh" && candidate.destination === "direct" ? connected : disconnected);
    const { result } = setup({ hosts: [multipleMethods] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.observation?.availability).toBe("available"));
    const observed = result.current.statuses.get(host.host_id)?.observation;

    act(() => result.current.connectionChanged(alternate, "connecting"));
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "connected", method_names: ["Direct"], observation: observed,
      operation: { kind: "connect", state: "pending", method_name: "VPN", message: null },
    });
    act(() => result.current.connectionChanged(alternate, "error", "VPN authentication failed"));
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "connected", method_names: ["Direct"], observation: observed,
      operation: { kind: "connect", state: "failed", message: "VPN authentication failed" },
    });
    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "connected", observation: { availability: "available", completeness: "complete" },
      operation: { state: "failed", message: "VPN authentication failed" },
    });
  });

  it.each([true, false])("represents a partial observation when a known method is connected=%s", async (available) => {
    vi.mocked(sshConnectionStatus).mockImplementation(async (candidate) => {
      if (candidate.kind === "ssh" && candidate.destination === "vpn") throw new Error("VPN status timed out");
      return available ? connected : disconnected;
    });
    const { result } = setup({ hosts: [multipleMethods] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.observation?.completeness).toBe("partial"));
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: available ? "connected" : "error",
      method_names: available ? ["Direct"] : [],
      observation: {
        availability: available ? "available" : "unknown", completeness: "partial",
        failed_method_names: ["VPN"], message: "VPN status timed out", stale: false,
        checked_at_ms: expect.any(Number),
      },
      operation: { state: "idle" },
    });
  });

  it("does not turn complete observation failure into known disconnection", async () => {
    vi.mocked(sshConnectionStatus).mockResolvedValue(connected);
    const { result } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("connected"));
    vi.mocked(sshConnectionStatus).mockRejectedValue(new Error("Broker status unavailable"));
    await act(async () => result.current.refresh());
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "error", method_names: [],
      observation: { availability: "unknown", completeness: "failed", failed_method_names: ["Direct"], stale: false },
      operation: { state: "idle", message: null },
    });
  });

  it("observes valid methods even when another method's gateway cannot be resolved", async () => {
    vi.mocked(sshConnectionStatus).mockResolvedValue(connected);
    const methods: WorkspaceHost = {
      ...multipleMethods,
      connection_methods: multipleMethods.connection_methods.map((method) => method.method_id === "vpn" ? {
        ...method, target: { ...method.target, gateway_route: [{ gateway_id: "missing", mode: "automatic" }] },
      } : method),
    };
    const { result } = setup({ hosts: [methods] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.observation?.completeness).toBe("partial"));
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "connected", method_names: ["Direct"],
      observation: { availability: "available", failed_method_names: ["VPN"] },
    });
    expect(sshConnectionStatus).toHaveBeenCalledOnce();
  });

  it("clears already published evidence while new settings await an observation", async () => {
    vi.mocked(sshConnectionStatus).mockResolvedValueOnce(connected);
    const { result, rerender, initial } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("connected"));
    const previousPoll = deferred<SshConnectionStatus>();
    const currentPoll = deferred<SshConnectionStatus>();
    vi.mocked(sshConnectionStatus).mockReturnValueOnce(previousPoll.promise).mockReturnValueOnce(currentPoll.promise);
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refresh(); });
    const replacement: WorkspaceHost = {
      ...host, connection_methods: [{
        method_id: "direct", name: "Updated route", target: { kind: "ssh", destination: "replacement" },
      }],
    };
    rerender({ ...initial, hosts: [replacement], targets: [{ ...target, destination: "replacement" }] });
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "checking", method_names: [],
      observation: { availability: "unknown", completeness: "pending", checked_at_ms: null, stale: true },
    });
    await act(async () => { previousPoll.resolve(connected); await refresh; });
    expect(result.current.statuses.get(host.host_id)?.observation?.availability).toBe("unknown");
    await act(async () => { currentPoll.resolve(connected); });
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "connected", method_names: ["Updated route"],
      observation: { availability: "available", completeness: "complete", stale: false },
    });
  });

  it("ignores connection completion and errors from an attempt whose configuration changed", async () => {
    const { result, rerender, initial } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("disconnected"));
    act(() => result.current.connectionChanged(target, "connecting"));
    const replacement = { ...host, name: "Updated host" };
    await act(async () => { rerender({ ...initial, hosts: [replacement] }); });
    act(() => result.current.connectionChanged(target, "error", "Old attempt failed"));
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "disconnected", operation: { state: "idle", message: null },
    });
    const queryCount = vi.mocked(sshConnectionStatus).mock.calls.length;
    act(() => result.current.connectionChanged(target, "connected"));
    expect(sshConnectionStatus).toHaveBeenCalledTimes(queryCount);
  });

  it("does not infer whole-host manual disconnection from a partial observation", async () => {
    vi.mocked(sshConnectionStatus).mockImplementation(async (candidate) => {
      if (candidate.kind === "ssh" && candidate.destination === "vpn") throw new Error("VPN status unavailable");
      return manuallyDisconnected;
    });
    const { result, initial } = setup({ hosts: [multipleMethods] });
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.observation?.completeness).toBe("partial"));
    expect(result.current.statuses.get(host.host_id)?.observation?.availability).toBe("unknown");
    expect(initial.onPause).not.toHaveBeenCalled();
    expect(result.current.isPaused(target)).toBe(false);
  });

  it.each(["success", "failure"] as const)("ignores stale disconnect %s after settings change", async (outcome) => {
    vi.mocked(sshConnectionStatus).mockResolvedValue(connected);
    const completion = deferred<void>();
    vi.mocked(disconnectSshHost).mockReturnValueOnce(completion.promise);
    const { result, rerender, initial } = setup();
    await waitFor(() => expect(result.current.statuses.get(host.host_id)?.state).toBe("connected"));
    let disconnect!: Promise<void>;
    await act(async () => { disconnect = result.current.disconnect(target); });
    await act(async () => { rerender({ ...initial, hosts: [{ ...host, name: "Updated host" }] }); });
    expect(result.current.statuses.get(host.host_id)?.observation?.availability).toBe("available");
    await act(async () => {
      if (outcome === "success") { completion.resolve(); await disconnect; }
      else { completion.reject(new Error("Old disconnect failed")); await expect(disconnect).rejects.toThrow("Old disconnect failed"); }
    });
    expect(result.current.statuses.get(host.host_id)).toMatchObject({
      state: "connected", operation: { state: "idle", message: null },
      observation: { availability: "available" },
    });
  });
});

it("bounds a stalled broker query and ignores its late connected result", async () => {
  vi.useFakeTimers();
  vi.mocked(sshConnectionStatus).mockResolvedValueOnce(connected);
  const { result } = setup();
  await act(async () => undefined);
  expect(result.current.statuses.get(host.host_id)?.observation?.availability).toBe("available");
  const stalled = deferred<SshConnectionStatus>();
  const current = deferred<SshConnectionStatus>();
  vi.mocked(sshConnectionStatus).mockReturnValueOnce(stalled.promise).mockReturnValue(current.promise);
  let refresh!: Promise<void>;
  act(() => { refresh = result.current.refresh(); });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(HOST_STATUS_TIMEOUT_MS);
    await refresh;
  });
  expect(result.current.statuses.get(host.host_id)).toMatchObject({
    state: "error", method_names: [],
    observation: { availability: "unknown", completeness: "failed", message: "SSH status query timed out." },
  });
  await act(async () => { stalled.resolve(connected); });
  expect(result.current.statuses.get(host.host_id)?.observation?.availability).toBe("unknown");
  await act(async () => { current.resolve(disconnected); });
  expect(result.current.statuses.get(host.host_id)?.observation?.availability).toBe("unavailable");
  // Only periodic refresh timers remain; query deadline timers were cleared.
  expect(vi.getTimerCount()).toBe(2);
});
