// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { GatewayRouteDialog } from "./GatewayRouteDialog";
import type { VpnSnapshot, WorkspaceSshGateway } from "../../lib/types";
import { openVpnSignIn, stopVpn, vpnStatus } from "../../lib/tauri";

vi.mock("../../lib/tauri", () => ({ openVpnSignIn: vi.fn(), stopVpn: vi.fn(), vpnStatus: vi.fn() }));

afterEach(cleanup);

const gateway: WorkspaceSshGateway = {
  gateway_id: "office-edge",
  name: "Office edge",
  destination: "office-edge.example",
  user: "operator",
  port: 2222,
};
const vpn = { connection_id: "office-vpn", name: "Office VPN", url: "https://vpn.example", username: "operator",
  has_password: true, auth_method: null, target_ip: null };

describe("GatewayRouteDialog", () => {
  it("converts a legacy VPN and existing gateways into the same ordered route", async () => {
    const route = [{ gateway_id: gateway.gateway_id, mode: "native_only" as const }];
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", vpn_connection_id: vpn.connection_id, gateway_route: route }}
      vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    expect(screen.getByText("2. Office edge")).toBeTruthy();
    expect(screen.getByLabelText("Connect through")).toHaveProperty("value", "gateway_route");
    await userEvent.setup().click(screen.getByRole("button", { name: "Done" }));
    expect(onSave).toHaveBeenCalledExactlyOnceWith([gateway], [{ vpn_connection_id: vpn.connection_id }, ...route]);
  });

  it("preserves a sole local VPN in the legacy field", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", vpn_connection_id: vpn.connection_id }}
      vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    expect(screen.getByText("VPN · Runs on This computer")).toBeTruthy();
    await userEvent.setup().click(screen.getByRole("button", { name: "Done" }));
    expect(onSave).toHaveBeenCalledExactlyOnceWith([gateway], [], vpn.connection_id);
  });

  it("appends an SSH gateway without clearing the selected local VPN", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", vpn_connection_id: vpn.connection_id }}
      vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(onSave).toHaveBeenCalledExactlyOnceWith([gateway], [
      { vpn_connection_id: vpn.connection_id }, { gateway_id: gateway.gateway_id, mode: "automatic" },
    ]);
  });

  it("builds SSH, VPN, SSH with VPN execution on the first jump host", async () => {
    const second = { ...gateway, gateway_id: "second", name: "Second edge" };
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", gateway_route: [{ gateway_id: gateway.gateway_id, mode: "native_only" }] }}
      vpn_connections={[vpn]} gateways={[gateway, second]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Add Office VPN to route" }));
    expect(screen.getByText("VPN · Runs on Office edge")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(onSave).toHaveBeenCalledExactlyOnceWith([gateway, second], [
      { gateway_id: gateway.gateway_id, mode: "native_only" }, { vpn_connection_id: vpn.connection_id },
      { gateway_id: second.gateway_id, mode: "automatic" },
    ]);
  });

  it("moves and removes VPN steps without losing SSH modes", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", gateway_route: [
      { gateway_id: gateway.gateway_id, mode: "native_only" }, { vpn_connection_id: vpn.connection_id },
    ] }} vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Move Office VPN up" }));
    expect(screen.getByText("VPN · Runs on This computer")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Remove Office VPN from route" }));
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(onSave).toHaveBeenCalledExactlyOnceWith([gateway], [{ gateway_id: gateway.gateway_id, mode: "native_only" }]);
  });

  it("rejects consecutive VPN steps created by rearranging a route", async () => {
    const otherVpn = { ...vpn, connection_id: "other", name: "Other VPN" };
    const onSave = vi.fn();
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", gateway_route: [
      { vpn_connection_id: vpn.connection_id }, { gateway_id: gateway.gateway_id, mode: "automatic" },
      { vpn_connection_id: otherVpn.connection_id },
    ] }} vpn_connections={[vpn, otherVpn]} gateways={[gateway]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Move Other VPN up" }));
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(screen.getByRole("alert").textContent).toContain("Consecutive VPN steps are not supported");
    expect(onSave).not.toHaveBeenCalled();
  });

  it("manages remote VPN status and disconnects only on its execution host", async () => {
    vi.mocked(vpnStatus).mockResolvedValue({ supports_multiple: true, connections: [{
      connection_id: vpn.connection_id, vpn_id: "remote-vpn", state: "connected", running: true,
      endpoint: "socks5h://127.0.0.1:1234", container_name: "vpn",
    }] });
    vi.mocked(stopVpn).mockResolvedValue({ connection_id: vpn.connection_id, state: "stopped", running: false, endpoint: null, container_name: null });
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", gateway_route: [
      { gateway_id: gateway.gateway_id, mode: "automatic" }, { vpn_connection_id: vpn.connection_id },
    ] }} vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={vi.fn()} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Check VPN status" }));
    expect(vpnStatus).toHaveBeenCalledWith({ kind: "ssh", destination: gateway.destination, user: gateway.user, port: gateway.port });
    await user.click(await screen.findByRole("button", { name: "Disconnect VPN" }));
    expect(stopVpn).toHaveBeenCalledWith("remote-vpn", { kind: "ssh", destination: gateway.destination, user: gateway.user, port: gateway.port });
    expect(openVpnSignIn).not.toHaveBeenCalled();
  });

  it("keeps missing remote VPN status unknown when inventory is incomplete", async () => {
    vi.mocked(vpnStatus).mockResolvedValueOnce({ supports_multiple: true, connections: [],
      discovery_warnings: ["Remote ctld is not running."] });
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", gateway_route: [
      { gateway_id: gateway.gateway_id, mode: "automatic" }, { vpn_connection_id: vpn.connection_id },
    ] }} vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={vi.fn()} onClose={vi.fn()} />);
    await userEvent.setup().click(screen.getByRole("button", { name: "Check VPN status" }));
    expect(await screen.findByText("Status unavailable on Office edge")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain("Remote ctld is not running");
    expect(screen.queryByText("Stopped on Office edge")).toBeNull();
    expect(screen.queryByRole("button", { name: "Disconnect VPN" })).toBeNull();
  });

  it("shows healthy selected VPN evidence alongside incomplete inventory warnings", async () => {
    vi.mocked(vpnStatus).mockResolvedValueOnce({ supports_multiple: true, connections: [{
      connection_id: vpn.connection_id, state: "connected", running: true,
      endpoint: "socks5h://127.0.0.1:1234", container_name: "vpn",
    }], discovery_warnings: ["Another container could not be inspected."] });
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", gateway_route: [
      { gateway_id: gateway.gateway_id, mode: "automatic" }, { vpn_connection_id: vpn.connection_id },
    ] }} vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={vi.fn()} onClose={vi.fn()} />);
    await userEvent.setup().click(screen.getByRole("button", { name: "Check VPN status" }));
    expect(await screen.findByText("Connected on Office edge")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain("Another container could not be inspected");
    expect(screen.getByRole("button", { name: "Disconnect VPN" })).toHaveProperty("disabled", false);
  });

  it("discards pending status and its runtime ID when the execution owner changes", async () => {
    let resolvePrevious!: (snapshot: VpnSnapshot) => void;
    vi.mocked(vpnStatus).mockReturnValueOnce(new Promise((resolve) => { resolvePrevious = resolve; }));
    vi.mocked(vpnStatus).mockResolvedValueOnce({ supports_multiple: true, connections: [{
      connection_id: vpn.connection_id, vpn_id: "new-owner-vpn", state: "connected", running: true,
      endpoint: "socks5h://127.0.0.1:4567", container_name: "new-owner",
    }] });
    vi.mocked(stopVpn).mockClear();
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", gateway_route: [
      { gateway_id: gateway.gateway_id, mode: "automatic" }, { vpn_connection_id: vpn.connection_id },
    ] }} vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={vi.fn()} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Check VPN status" }));
    await user.click(screen.getAllByRole("button", { name: "Edit" })[0]);
    await user.clear(screen.getByLabelText("SSH destination / alias"));
    await user.type(screen.getByLabelText("SSH destination / alias"), "replacement.example");
    await user.click(screen.getByRole("button", { name: "Save gateway" }));
    await act(async () => { resolvePrevious({ supports_multiple: true, connections: [{
      connection_id: vpn.connection_id, vpn_id: "old-owner-vpn", state: "connected", running: true,
      endpoint: "socks5h://127.0.0.1:1234", container_name: "old-owner",
    }] }); });
    expect(screen.queryByRole("button", { name: "Disconnect VPN" })).toBeNull();
    expect(screen.getByRole("button", { name: "Check VPN status" })).toHaveProperty("disabled", false);
    await user.click(screen.getByRole("button", { name: "Check VPN status" }));
    await user.click(await screen.findByRole("button", { name: "Disconnect VPN" }));
    expect(stopVpn).toHaveBeenCalledExactlyOnceWith("new-owner-vpn", {
      kind: "ssh", destination: "replacement.example", user: gateway.user, port: gateway.port,
    });
  });

  it("requires an explicit replacement when the saved VPN is missing", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", vpn_connection_id: "removed-vpn" }}
      gateways={[gateway]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    expect(screen.getByLabelText("Connect through")).toHaveProperty("value", "vpn:removed-vpn");
    expect(screen.getByRole("button", { name: "Done" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("alert").textContent).toContain("saved VPN is unavailable");
    const user = userEvent.setup();
    await user.selectOptions(screen.getByLabelText("Connect through"), "direct");
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(onSave).toHaveBeenCalledExactlyOnceWith([gateway], []);
  });

  it("preserves the order and modes of an existing multi-hop route", async () => {
    const second = { ...gateway, gateway_id: "second", name: "Second edge" };
    const route = [{ gateway_id: gateway.gateway_id, mode: "native_only" as const }, { gateway_id: second.gateway_id, mode: "automatic" as const }];
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", gateway_route: route }}
      gateways={[gateway, second]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    expect(screen.getByLabelText("Connect through")).toHaveProperty("value", "gateway_route");
    await userEvent.setup().click(screen.getByRole("button", { name: "Done" }));
    expect(onSave).toHaveBeenCalledExactlyOnceWith([gateway, second], route);
  });

  it("keeps gateway mode controls in keyboard navigation and cancels with Escape", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<GatewayRouteDialog
      target={{ kind: "ssh", destination: "server", gateway_route: [{ gateway_id: gateway.gateway_id, mode: "automatic" }] }}
      gateways={[gateway]} targets={[]} onSave={vi.fn()} onClose={onClose} />);
    screen.getByRole("button", { name: "Remove Office edge from route" }).focus();
    await user.tab();
    expect(document.activeElement).toBe(screen.getByLabelText("Connection to next host"));
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("adds a reusable gateway to the ordered route", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(
      <GatewayRouteDialog
        target={{ kind: "ssh", host_id: "server", destination: "server.internal" }}
        gateways={[gateway]}
        targets={[]}
        onSave={onSave}
        onClose={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(screen.getByText("1. Office edge")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Done" }));

    await waitFor(() => expect(onSave).toHaveBeenCalledWith(
      [gateway],
      [{ gateway_id: gateway.gateway_id, mode: "automatic" }],
    ));
  });

  it("creates a saved gateway and adds it to this route", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(
      <GatewayRouteDialog
        target={{ kind: "ssh", host_id: "server", destination: "server.internal" }}
        gateways={[]}
        targets={[]}
        onSave={onSave}
        onClose={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "+ New gateway" }));
    await user.type(screen.getByLabelText("Name"), "Bastion");
    await user.type(screen.getByLabelText("SSH destination / alias"), "bastion.example");
    await user.click(screen.getByRole("button", { name: "Save gateway" }));
    expect(screen.getByText("1. Bastion")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Done" }));

    await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
    const [gateways, route] = onSave.mock.calls[0];
    expect(gateways).toMatchObject([
      { name: "Bastion", destination: "bastion.example" },
    ]);
    expect(route).toEqual([
      { gateway_id: gateways[0].gateway_id, mode: "automatic" },
    ]);
  });

  it("does not delete a gateway while another host references it", () => {
    render(
      <GatewayRouteDialog
        target={{ kind: "ssh", host_id: "server", destination: "server.internal" }}
        gateways={[gateway]}
        targets={[{
          kind: "ssh",
          host_id: "other",
          destination: "other.internal",
          gateway_route: [{ gateway_id: gateway.gateway_id, mode: "native_only" }],
        }]}
        onSave={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(
      (screen.getByRole("button", { name: "Delete" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });
  it("adds a SOCKS5 hop with proxy credentials requested at connection time", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog
      target={{ kind: "ssh", host_id: "server", destination: "server.internal" }}
      gateways={[]}
      targets={[]}
      onSave={onSave}
      onClose={vi.fn()}
    />);

    await user.click(screen.getByRole("button", { name: "+ New gateway" }));
    await user.selectOptions(screen.getByLabelText("Type"), "socks5");
    await user.type(screen.getByLabelText("Name"), "Proxy");
    await user.type(screen.getByLabelText("SOCKS5 proxy address"), "proxy.internal");
    await user.type(screen.getByLabelText("Username (optional)"), "alice");
    await user.type(screen.getByLabelText("Port"), "1080");
    await user.click(screen.getByRole("button", { name: "Save gateway" }));
    await user.click(screen.getByRole("button", { name: "Done" }));

    await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
    expect(onSave.mock.calls[0][0]).toMatchObject([{
      kind: "socks5", name: "Proxy", destination: "proxy.internal", user: "alice", port: 1080,
    }]);
    expect(onSave.mock.calls[0][1]).toHaveLength(1);
  });

});
