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
});
