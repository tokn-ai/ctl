// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { disconnectSshHost, probeSshHost, sshConnectionStatus } from "../../lib/tauri";
import type { SshConnectionStatus, SshConnectionTarget, WorkspaceHost } from "../../lib/types";
import { HOST_STATUS_INTERVAL_MS, useHostConnections } from "./useHostConnections";

vi.mock("../../lib/tauri", () => ({
  disconnectSshHost: vi.fn(),
  probeSshHost: vi.fn(),
  sshConnectionStatus: vi.fn(),
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
  vi.mocked(disconnectSshHost).mockResolvedValue(undefined);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("live host connections", () => {
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
    expect(initial.onResume).toHaveBeenCalledOnce();
  });
});
