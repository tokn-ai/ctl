// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceHost } from "../../lib/types";
import { HostSettingsDialog } from "./HostSettingsDialog";

const host: WorkspaceHost = {
  host_id: "build-machine",
  name: "Build machine",
  preferred_method_id: "lan",
  connection_methods: [
    { method_id: "lan", name: "Office LAN", target: { kind: "ssh", destination: "builder", hostname: "10.0.0.8", user: "deploy" } },
    { method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "builder-vpn", gateway_route: [{ gateway_id: "office", mode: "automatic" }] } },
  ],
};

afterEach(cleanup);

function setup(saved = host) {
  const callbacks = {
    onSave: vi.fn(async (_host: WorkspaceHost) => undefined),
    onAddMethod: vi.fn(),
    onEditMethod: vi.fn(),
    onConnect: vi.fn(),
    onClose: vi.fn(),
  };
  render(<HostSettingsDialog host={saved} {...callbacks} />);
  return { ...callbacks, user: userEvent.setup() };
}

describe("host settings", () => {
  it("explains that customizing a virtual Tailscale host saves it in the host catalog", () => {
    const { onSave } = setup({ ...host, source: "tailscale" });
    expect(screen.getByText("Discovered from Tailscale. Customizing this virtual host saves it in hosts.json.")).toBeTruthy();
    expect(onSave).not.toHaveBeenCalled();
  });

  it("saves names and preferred method together without connecting or changing method identities", async () => {
    const { user, onSave, onClose, onConnect } = setup();
    await user.clear(screen.getByLabelText("Host name"));
    await user.type(screen.getByLabelText("Host name"), "Build machine in office");
    const vpn = within(screen.getByRole("region", { name: "VPN" }));
    await user.click(vpn.getByRole("button", { name: "Make preferred" }));
    await user.clear(vpn.getByRole("textbox"));
    await user.type(vpn.getByRole("textbox"), "Via office gateway");
    expect(onSave).not.toHaveBeenCalled();
    expect((vpn.getByRole("button", { name: "Connect using" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
    expect(onSave.mock.calls[0][0]).toEqual({
      ...host,
      name: "Build machine in office",
      preferred_method_id: "vpn",
      connection_methods: [host.connection_methods[0], { ...host.connection_methods[1], name: "Via office gateway" }],
    });
    expect(onClose).toHaveBeenCalledOnce();
    expect(onConnect).not.toHaveBeenCalled();
    expect(host.name).toBe("Build machine");
  });

  it("makes removing the preferred method an explicit save and keeps the last method", async () => {
    const { user, onSave } = setup();
    await user.click(within(screen.getByRole("region", { name: "Office LAN" })).getByRole("button", { name: "Remove" }));
    expect(screen.queryByRole("region", { name: "Office LAN" })).toBeNull();
    const vpn = within(screen.getByRole("region", { name: "VPN" }));
    expect(vpn.getByText("Preferred")).toBeTruthy();
    expect((vpn.getByRole("button", { name: "Remove" }) as HTMLButtonElement).disabled).toBe(true);
    expect(onSave).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(onSave).toHaveBeenCalledWith({ ...host, preferred_method_id: "vpn", connection_methods: [host.connection_methods[1]] });
  });

  it("launches operations only for the selected saved method and cancels draft changes", async () => {
    const { user, onConnect, onEditMethod, onAddMethod, onSave, onClose } = setup();
    const vpn = within(screen.getByRole("region", { name: "VPN" }));
    await user.click(vpn.getByRole("button", { name: "Connect using" }));
    await user.click(vpn.getByRole("button", { name: "Edit connection" }));
    await user.click(screen.getByRole("button", { name: "Add connection" }));
    expect(onConnect).toHaveBeenCalledWith(host.connection_methods[1]);
    expect(onEditMethod).toHaveBeenCalledWith(host.connection_methods[1]);
    expect(onAddMethod).toHaveBeenCalledOnce();
    await user.type(screen.getByLabelText("Host name"), " changed");
    await user.keyboard("{Escape}");
    expect(onSave).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("retains the draft when saving fails and can retry", async () => {
    const { user, onSave, onClose } = setup();
    onSave.mockRejectedValueOnce(new Error("Workspace changed externally"));
    await user.type(screen.getByLabelText("Host name"), " updated");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Workspace changed externally");
    expect(onClose).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(onSave).toHaveBeenCalledTimes(2);
    expect(onClose).toHaveBeenCalledOnce();
  });
});
