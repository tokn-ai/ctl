// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
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
import type { SshRemotePlatform, TcpListener, WorkspacePortForward } from "../../lib/types";

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
  it("offers manual TCP forwarding for Windows SSH hosts", () => {
    renderDialog(vi.fn(), "windows");
    expect(screen.getByRole("combobox", { name: "Protocol" })).toHaveProperty("value", "tcp");
    expect(screen.queryByRole("option", { name: "UDP" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Remote TCP/UDP listeners" })).toBeNull();
    expect(listRemoteListeners).not.toHaveBeenCalled();
  });

  it("keeps failed forward operations on their row and preserves an unrelated form correction", async () => {
    vi.mocked(listRemoteListeners).mockResolvedValue({ listeners: [], warnings: [] });
    const onSetEnabled = vi.fn().mockRejectedValueOnce(new Error("SSH unavailable")).mockResolvedValue(undefined);
    const user = userEvent.setup();
    const forward: WorkspacePortForward = { forward_id: "database", host_id: "remote", name: "Database", bind_address: "127.0.0.1", local_port: 5432, remote_host: "127.0.0.1", remote_port: 5432, enabled: false };
    render(<PortForwardingDialog target={{ kind: "ssh", host_id: "remote", destination: "example" }} forwards={[forward]} statuses={new Map()} busy={new Set()} onChange={vi.fn()} onSetEnabled={onSetEnabled} onUpdateAgent={vi.fn()} onClose={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add stopped forward" }));
    const row = within(screen.getByText("Database").closest(".port-forward-row") as HTMLElement);
    await user.click(row.getByRole("button", { name: "Start" }));
    expect(onSetEnabled).toHaveBeenCalledWith(forward, true, { feedback: "inline" });
    expect(row.getByRole("alert").textContent).toContain("SSH unavailable");
    expect(screen.getAllByRole("alert")).toHaveLength(2);
    await user.click(row.getByRole("button", { name: "Retry forward" }));
    expect(row.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("alert").textContent).toContain("Enter a name, remote host, and valid ports");
  });

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

function renderDialog(onChange: (forwards: WorkspacePortForward[]) => void, remote_platform?: SshRemotePlatform) {
  return render(
    <PortForwardingDialog
      target={{ kind: "ssh", host_id: "remote", destination: "example", remote_platform }}
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
