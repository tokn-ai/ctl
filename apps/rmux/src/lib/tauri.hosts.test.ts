import { beforeEach, describe, expect, it, vi } from "vitest";
import { configurePortForward, createSession, listSessions, probeSshHost } from "./tauri";
import type { LocalPortForward, SshConnectionTarget } from "./types";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: ipc.invoke,
  Channel: class { onmessage = (_message: unknown) => {}; },
}));

const target: SshConnectionTarget = {
  kind: "ssh", host_id: "ssh-config:work", destination: "work",
  unavailable: "The SSH config alias work is missing.",
};
const forward: LocalPortForward = {
  forward_id: "database", bind_address: "127.0.0.1", local_port: 5432,
  remote_host: "127.0.0.1", remote_port: 5432,
};

beforeEach(() => vi.resetAllMocks());

describe("unavailable host transport boundary", () => {
  it("does not send a vanished SSH alias to native connection or discovery commands", async () => {
    await expect(probeSshHost(target, "attempt", vi.fn())).rejects.toThrow(target.unavailable);
    await expect(listSessions(target)).rejects.toThrow(target.unavailable);
    await expect(createSession({
      target, working_directory: null,
      terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
    })).rejects.toThrow(target.unavailable);
    await expect(configurePortForward(target, forward, true)).rejects.toThrow(target.unavailable);
    expect(ipc.invoke).not.toHaveBeenCalled();
  });

  it("can still stop a forward through the daemon's retained owner", async () => {
    ipc.invoke.mockResolvedValue({ forward, state: "waiting_for_authentication", message: null });
    await configurePortForward(target, forward, false);
    expect(ipc.invoke).toHaveBeenCalledWith("configure_port_forward", {
      request: { target, forward, enabled: false },
    });
  });
});
