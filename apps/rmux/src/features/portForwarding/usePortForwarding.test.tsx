// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  PortForwardStatus,
  SshConnectionTarget,
  WorkspacePortForward,
} from "../../lib/types";
import { configurePortForward, listPortForwards } from "../../lib/tauri";
import { usePortForwarding } from "./usePortForwarding";

vi.mock("../../lib/tauri", () => ({
  configurePortForward: vi.fn(),
  listPortForwards: vi.fn(),
}));

const target: SshConnectionTarget = {
  kind: "ssh",
  host_id: "remote",
  destination: "example",
};
const forward: WorkspacePortForward = {
  forward_id: "database",
  host_id: "remote",
  name: "Database",
  bind_address: "127.0.0.1",
  local_port: 5432,
  remote_host: "127.0.0.1",
  remote_port: 5432,
  enabled: true,
};
const active: PortForwardStatus = {
  forward,
  state: "active",
  message: null,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(configurePortForward).mockResolvedValue(active);
  vi.mocked(listPortForwards).mockResolvedValue([active]);
});
afterEach(cleanup);

describe("port forwarding controller", () => {
  it("restores enabled forwards and publishes their runtime status", async () => {
    const { result } = renderHook(() =>
      usePortForwarding(true, [target], [forward], vi.fn()),
    );

    await waitFor(() => expect(result.current.refreshing).toBe(false));
    await waitFor(() =>
      expect(result.current.statuses.get("database")?.state).toBe("active"),
    );
    expect(configurePortForward).toHaveBeenCalledWith(target, forward, true);
    expect(listPortForwards).toHaveBeenCalledWith(target);
    expect(result.current.lastRefreshedAt).not.toBeNull();
  });

  it("persists a successful state change and exposes a failed host operation", async () => {
    const update = vi.fn();
    const { result } = renderHook(() =>
      usePortForwarding(false, [target], [forward], update),
    );

    await act(async () => result.current.setEnabled(target, forward, false));
    expect(update).toHaveBeenCalledOnce();
    expect(update.mock.calls[0][0]([forward])[0].enabled).toBe(false);
    expect(result.current.statuses.has("database")).toBe(false);

    vi.mocked(configurePortForward).mockRejectedValueOnce(new Error("offline"));
    let failure: unknown;
    await act(async () => {
      try {
        await result.current.setEnabled(target, forward, true);
      } catch (error) {
        failure = error;
      }
    });
    expect(failure).toEqual(new Error("offline"));
    expect(result.current.hostErrors.get("remote")).toBe("offline");
  });

  it("finishes an old in-flight configure before moving forwards, without continuing its stale batch", async () => {
    const oldConfigure = deferred<PortForwardStatus>();
    vi.mocked(configurePortForward).mockReturnValueOnce(oldConfigure.promise);
    const alternate = { ...target, destination: "via-vpn", method_id: "vpn" };
    const other = { ...forward, forward_id: "web", local_port: 8080, remote_port: 80 };
    const statuses: PortForwardStatus[] = [active, { ...active, forward: other }];
    vi.mocked(listPortForwards).mockResolvedValue(statuses);
    const { result, rerender } = renderHook(({ current_target }) =>
      usePortForwarding(false, [current_target], [forward, other], vi.fn()),
    { initialProps: { current_target: target } });
    let oldRefresh!: Promise<void>;
    act(() => { oldRefresh = result.current.refreshTarget(target); });
    await waitFor(() => expect(configurePortForward).toHaveBeenCalledOnce());

    rerender({ current_target: alternate });
    let newRefresh!: Promise<void>;
    act(() => { newRefresh = result.current.refreshTarget(alternate); });
    expect(configurePortForward).toHaveBeenCalledOnce();
    await act(async () => {
      oldConfigure.resolve(active);
      await Promise.all([oldRefresh, newRefresh]);
    });

    expect(vi.mocked(configurePortForward).mock.calls).toEqual([
      [target, forward, true],
      [target, forward, false],
      [alternate, forward, true],
      [alternate, other, true],
    ]);
    expect(listPortForwards).toHaveBeenCalledExactlyOnceWith(alternate);
    expect([...result.current.statuses.values()]).toEqual(statuses);
  });

  it.each(["success", "error"] as const)("suppresses a late old-method list %s after switching methods", async (outcome) => {
    const oldList = deferred<PortForwardStatus[]>();
    const newList = deferred<PortForwardStatus[]>();
    vi.mocked(listPortForwards)
      .mockReturnValueOnce(oldList.promise)
      .mockReturnValueOnce(newList.promise);
    const alternate = { ...target, destination: "via-vpn", method_id: "vpn" };
    const { result, rerender } = renderHook(({ current_target }) =>
      usePortForwarding(false, [current_target], [forward], vi.fn()),
    { initialProps: { current_target: target } });
    let oldRefresh!: Promise<void>;
    act(() => { oldRefresh = result.current.refreshTarget(target); });
    await waitFor(() => expect(listPortForwards).toHaveBeenCalledOnce());

    rerender({ current_target: alternate });
    let newRefresh!: Promise<void>;
    act(() => { newRefresh = result.current.refreshTarget(alternate); });
    await act(async () => {
      if (outcome === "error") oldList.reject(new Error("Old route unavailable"));
      else oldList.resolve([{ ...active, state: "waiting_for_authentication", message: "Old route" }]);
      await oldRefresh;
    });
    await waitFor(() => expect(listPortForwards).toHaveBeenCalledTimes(2));
    expect(result.current.statuses.size).toBe(0);
    expect(result.current.hostErrors.size).toBe(0);
    await act(async () => {
      newList.resolve([active]);
      await newRefresh;
    });
    expect(result.current.statuses.get(forward.forward_id)).toEqual(active);
    expect(result.current.hostErrors.size).toBe(0);
  });

  it("does not let an old-method enable persist or leave an active forward after a method switch", async () => {
    const oldConfigure = deferred<PortForwardStatus>();
    vi.mocked(configurePortForward).mockReturnValueOnce(oldConfigure.promise);
    vi.mocked(listPortForwards).mockResolvedValue([]);
    const disabled = { ...forward, enabled: false };
    const alternate = { ...target, destination: "via-vpn", method_id: "vpn" };
    const update = vi.fn();
    const { result, rerender } = renderHook(({ current_target }) =>
      usePortForwarding(false, [current_target], [disabled], update),
    { initialProps: { current_target: target } });
    let enabling!: Promise<void>;
    act(() => { enabling = result.current.setEnabled(target, disabled, true); });
    await waitFor(() => expect(configurePortForward).toHaveBeenCalledOnce());
    expect(result.current.busy.has(forward.forward_id)).toBe(true);

    rerender({ current_target: alternate });
    let refresh!: Promise<void>;
    act(() => { refresh = result.current.refreshTarget(alternate); });
    await act(async () => {
      oldConfigure.resolve(active);
      await Promise.all([enabling, refresh]);
    });

    expect(vi.mocked(configurePortForward).mock.calls).toEqual([
      [target, disabled, true],
      [target, disabled, false],
    ]);
    expect(listPortForwards).toHaveBeenCalledExactlyOnceWith(alternate);
    expect(update).not.toHaveBeenCalled();
    expect(result.current.statuses.size).toBe(0);
    expect(result.current.busy.size).toBe(0);
  });

  it("keeps distinct forward changes on the same method while serializing them", async () => {
    const firstConfigure = deferred<PortForwardStatus>();
    vi.mocked(configurePortForward).mockReturnValueOnce(firstConfigure.promise);
    const other = { ...forward, forward_id: "web", local_port: 8080, remote_port: 80 };
    const update = vi.fn();
    const { result } = renderHook(() =>
      usePortForwarding(false, [target], [forward, other], update));
    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => {
      first = result.current.setEnabled(target, forward, false);
      second = result.current.setEnabled(target, other, false);
    });
    await waitFor(() => expect(configurePortForward).toHaveBeenCalledOnce());
    await act(async () => {
      firstConfigure.resolve(active);
      await Promise.all([first, second]);
    });
    expect(vi.mocked(configurePortForward).mock.calls).toEqual([
      [target, forward, false], [target, other, false],
    ]);
    expect(update).toHaveBeenCalledTimes(2);
    expect(result.current.busy.size).toBe(0);
  });

  it("uses the selected method for a stale dialog toggle queued before the next render", async () => {
    const pendingList = deferred<PortForwardStatus[]>();
    vi.mocked(listPortForwards).mockReturnValueOnce(pendingList.promise);
    const alternate = { ...target, destination: "via-vpn", method_id: "vpn" };
    const disabled = { ...forward, enabled: false };
    const update = vi.fn();
    const { result } = renderHook(() =>
      usePortForwarding(false, [target], [disabled], update));
    let refreshing!: Promise<void>;
    act(() => { refreshing = result.current.refreshTarget(alternate); });
    await waitFor(() => expect(listPortForwards).toHaveBeenCalledOnce());

    let enabling!: Promise<void>;
    act(() => { enabling = result.current.setEnabled(target, disabled, true); });
    expect(configurePortForward).not.toHaveBeenCalled();
    await act(async () => {
      pendingList.resolve([]);
      await Promise.all([refreshing, enabling]);
    });
    expect(configurePortForward).toHaveBeenCalledExactlyOnceWith(alternate, disabled, true);
    expect(update).toHaveBeenCalledOnce();
    expect(update.mock.calls[0][0]([disabled])[0].enabled).toBe(true);
  });

  it("keeps a background refresh on the selected method while runtime props still show the previous one", async () => {
    const pendingConfigure = deferred<PortForwardStatus>();
    vi.mocked(configurePortForward).mockReturnValueOnce(pendingConfigure.promise);
    const alternate = { ...target, destination: "via-vpn", method_id: "vpn" };
    const { result } = renderHook(() =>
      usePortForwarding(false, [target], [forward], vi.fn()));
    let selectedRefresh!: Promise<void>;
    act(() => { selectedRefresh = result.current.refreshTarget(alternate); });
    await waitFor(() => expect(configurePortForward).toHaveBeenCalledOnce());
    let backgroundRefresh!: Promise<void>;
    act(() => { backgroundRefresh = result.current.refreshAll(); });
    await act(async () => {
      pendingConfigure.resolve(active);
      await Promise.all([selectedRefresh, backgroundRefresh]);
    });

    expect(vi.mocked(configurePortForward).mock.calls).toEqual([
      [alternate, forward, true], [alternate, forward, true],
    ]);
    expect(vi.mocked(listPortForwards).mock.calls).toEqual([[alternate], [alternate]]);
    expect(result.current.statuses.get(forward.forward_id)).toEqual(active);
  });

  it("does not reconnect a removed host through a stale refresh or toggle", async () => {
    const { result, rerender } = renderHook(({ targets }) =>
      usePortForwarding(false, targets, [forward], vi.fn()),
    { initialProps: { targets: [target] } });
    rerender({ targets: [] });

    await act(async () => {
      await result.current.refreshTarget(target);
      await expect(result.current.setEnabled(target, forward, true))
        .rejects.toThrow("host is no longer in the workspace");
    });
    expect(configurePortForward).not.toHaveBeenCalled();
    expect(listPortForwards).not.toHaveBeenCalled();
  });
});
