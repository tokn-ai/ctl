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
});
