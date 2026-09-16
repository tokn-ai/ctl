// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  PortForwardingDialog,
  listenerScope,
  remoteHostForListener,
} from "./PortForwardingDialog";
import {
  checkLocalPort,
  configurePortForward,
  listPortForwards,
  listRemoteListeners,
} from "../../lib/tauri";

vi.mock("../../lib/tauri", () => ({
  checkLocalPort: vi.fn(),
  configurePortForward: vi.fn(),
  listPortForwards: vi.fn(),
  listRemoteListeners: vi.fn(),
}));

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(listPortForwards).mockResolvedValue([]);
  vi.mocked(configurePortForward).mockResolvedValue({
    forward: {
      forward_id: "unused",
      bind_address: "127.0.0.1",
      local_port: 1,
      remote_host: "127.0.0.1",
      remote_port: 1,
    },
    state: "active",
    message: null,
  });
  vi.mocked(checkLocalPort).mockResolvedValue({
    port: 1,
    available: true,
    message: null,
  });
});

describe("remote listener forwarding defaults", () => {
  it("connects wildcard listeners through the matching loopback family", () => {
    expect(remoteHostForListener("0.0.0.0")).toBe("127.0.0.1");
    expect(remoteHostForListener("::")).toBe("::1");
    expect(remoteHostForListener("10.0.0.5")).toBe("10.0.0.5");
  });

  it("labels listener exposure without process inspection", () => {
    expect(listenerScope("127.0.0.1")).toBe("Loopback");
    expect(listenerScope("::1")).toBe("Loopback");
    expect(listenerScope("0.0.0.0")).toBe("All interfaces");
    expect(listenerScope("192.168.1.2")).toBe("Specific interface");
  });

  it("offers the update action when the remote agent lacks listener discovery", async () => {
    vi.mocked(listRemoteListeners).mockRejectedValue({
      code: "ctl_agent_update_required",
      message: "remote components must be updated",
    });
    const onUpdateAgent = vi.fn();
    const user = userEvent.setup();
    render(
      <PortForwardingDialog
        target={{ kind: "ssh", host_id: "remote", destination: "example" }}
        forwards={[]}
        onChange={vi.fn()}
        onUpdateAgent={onUpdateAgent}
        onClose={vi.fn()}
      />,
    );

    const update = await screen.findByRole("button", { name: "Update remote components" });
    expect(screen.queryByText("No visible TCP listeners.")).toBeNull();
    await user.click(update);
    await waitFor(() => expect(onUpdateAgent).toHaveBeenCalledOnce());
  });
});
