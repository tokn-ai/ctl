// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { GatewayRouteDialog } from "./GatewayRouteDialog";
import type { WorkspaceSshGateway } from "../../lib/types";

afterEach(cleanup);

const gateway: WorkspaceSshGateway = {
  gateway_id: "office-edge",
  name: "Office edge",
  destination: "office-edge.example",
  user: "operator",
  port: 2222,
};

describe("GatewayRouteDialog", () => {
  it("preserves existing gateways behind a VPN when the route is saved unchanged", async () => {
    const vpn = { connection_id: "office-vpn", name: "Office VPN", url: "https://vpn.example", username: "operator",
      has_password: true, auth_method: null, target_ip: null };
    const route = [{ gateway_id: gateway.gateway_id, mode: "native_only" as const }];
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<GatewayRouteDialog target={{ kind: "ssh", destination: "build", vpn_connection_id: vpn.connection_id, gateway_route: route }}
      vpn_connections={[vpn]} gateways={[gateway]} targets={[]} onSave={onSave} onClose={vi.fn()} />);
    expect(screen.getByText("1. Office edge")).toBeTruthy();
    expect(screen.getByLabelText("Connect through")).toHaveProperty("value", `vpn:${vpn.connection_id}`);
    await userEvent.setup().click(screen.getByRole("button", { name: "Done" }));
    expect(onSave).toHaveBeenCalledExactlyOnceWith([gateway], route, vpn.connection_id);
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
