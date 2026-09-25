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
  listRemoteListeners,
} from "../../lib/tauri";

vi.mock("../../lib/tauri", () => ({
  checkLocalPort: vi.fn(),
  listRemoteListeners: vi.fn(),
}));

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
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
        statuses={new Map()}
        busy={new Set()}
        onChange={vi.fn()}
        onSetEnabled={vi.fn()}
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
