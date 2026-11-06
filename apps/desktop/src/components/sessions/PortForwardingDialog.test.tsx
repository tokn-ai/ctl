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
import type { TcpListener, WorkspacePortForward } from "../../lib/types";

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
  it.each([
    ["tcp", "tcp"],
    ["udp", "udp"],
    [undefined, "tcp"],
  ] as const)("preserves a discovered %s listener's protocol", async (discovered_protocol, expected_protocol) => {
    const listener: TcpListener = { bind_address: "0.0.0.0", port: 5353, protocol: discovered_protocol };
    vi.mocked(listRemoteListeners).mockResolvedValue({ listeners: [listener], warnings: [] });
    const onChange = vi.fn();
    const user = userEvent.setup();
    renderDialog(onChange);

    await user.click(await screen.findByRole("button", { name: /0.0.0.0:5353/ }));
    expect((screen.getByRole("combobox", { name: "Protocol" }) as HTMLSelectElement).value).toBe(expected_protocol);
    await waitFor(() => expect(checkLocalPort).toHaveBeenCalledWith(5353, expected_protocol));
    await user.click(screen.getByRole("button", { name: "Add stopped forward" }));
    expect(onChange).toHaveBeenCalledWith([expect.objectContaining({
      local_port: 5353, remote_host: "127.0.0.1", remote_port: 5353, protocol: expected_protocol,
    })]);
  });

  it("shows TCP and UDP at the same address separately and allows forwarding both", async () => {
    vi.mocked(listRemoteListeners).mockResolvedValue({
      listeners: ["tcp", "udp"].map((protocol) => ({
        bind_address: "127.0.0.1", port: 5353, protocol: protocol as "tcp" | "udp",
      })),
      warnings: [],
    });
    const onChange = vi.fn();
    const user = userEvent.setup();
    renderDialog(onChange);

    expect(await screen.findByRole("button", { name: /127.0.0.1:5353.*TCP/ })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /127.0.0.1:5353.*UDP/ }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Protocol" }), "any");
    await waitFor(() => expect(checkLocalPort).toHaveBeenCalledWith(5353, "any"));
    await user.click(screen.getByRole("button", { name: "Add stopped forward" }));
    expect(onChange).toHaveBeenCalledWith([expect.objectContaining({ protocol: "any" })]);
  });

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
    expect(screen.queryByText("No visible TCP/UDP listeners.")).toBeNull();
    await user.click(update);
    await waitFor(() => expect(onUpdateAgent).toHaveBeenCalledOnce());
  });
});

function renderDialog(onChange: (forwards: WorkspacePortForward[]) => void) {
  return render(
    <PortForwardingDialog
      target={{ kind: "ssh", host_id: "remote", destination: "example" }}
      forwards={[]}
      statuses={new Map()}
      busy={new Set()}
      onChange={onChange}
      onSetEnabled={vi.fn()}
      onUpdateAgent={vi.fn()}
      onClose={vi.fn()}
    />,
  );
}
