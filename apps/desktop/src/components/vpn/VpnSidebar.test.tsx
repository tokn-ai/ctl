// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VpnController } from "../../features/vpn/useVpn";
import type { VpnConnection } from "../../lib/types";
import { VpnSidebar } from "./VpnSidebar";

const connection: VpnConnection = {
  connection_id: "work", name: "Work", url: "https://vpn.example.test", username: "example-user",
  has_password: true, auth_method: null, target_ip: null,
};
function model(overrides: Partial<VpnController> = {}): VpnController {
  return {
    connections: [connection], catalog_loaded: true, catalog_loading: false, catalog_error: null,
    status: { endpoint: null, container_name: null, connection_id: null, running: false, state: "stopped" },
    status_loaded: true, status_loading: false, status_stale: false, status_error: null, last_checked_at: null,
    action: null, action_error: null, profile_busy: false, deleting_id: null,
    editor: null, editor_error: null, editor_saving: false,
    refresh: vi.fn().mockResolvedValue(undefined), connect: vi.fn().mockResolvedValue(undefined), stop: vi.fn().mockResolvedValue(undefined),
    addConnection: vi.fn(), editConnection: vi.fn(), closeEditor: vi.fn(),
    saveConnection: vi.fn().mockResolvedValue(true), deleteConnection: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}
afterEach(cleanup);

describe("VPN sidebar", () => {
  it("manages named connections without displaying a password or env-file control", async () => {
    const user = userEvent.setup();
    const state = model();
    render(<VpnSidebar model={state} />);
    await user.click(screen.getByRole("button", { name: "Connect Work" }));
    expect(state.connect).toHaveBeenCalledWith("work");
    await user.click(screen.getByRole("button", { name: "Edit Work" }));
    expect(state.editConnection).toHaveBeenCalledWith(connection);
    await user.click(screen.getByRole("button", { name: "Delete Work" }));
    expect(state.deleteConnection).toHaveBeenCalledWith("work");
    expect(screen.queryByText(/env.file/i)).toBeNull();
    expect(screen.queryByLabelText("Password")).toBeNull();
  });

  it("shows, copies, and disconnects a CLI VPN even when saved profiles cannot load", async () => {
    const user = userEvent.setup();
    const clipboard = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue(undefined);
    const state = model({
      connections: [], catalog_loaded: false, catalog_error: "Saved connections unavailable",
      status: {
        endpoint: "socks5h://127.0.0.1:49152", container_name: "test-vpn", connection_id: null,
        vpn_url: "https://gateway.example.test", username: "connected-user", running: true, state: "connected",
      },
    });
    render(<VpnSidebar model={state} />);
    expect(screen.getByText("Connected VPN")).toBeTruthy();
    expect(screen.getByText("VPN server")).toBeTruthy();
    expect(screen.getByText("https://gateway.example.test")).toBeTruthy();
    expect(screen.getByText("Username")).toBeTruthy();
    expect(screen.getByText("connected-user")).toBeTruthy();
    expect(screen.getByText("SOCKS5 endpoint")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Copy SOCKS endpoint" }));
    expect(clipboard).toHaveBeenCalledWith("socks5h://127.0.0.1:49152");
    await user.click(screen.getByRole("button", { name: "Disconnect VPN" }));
    expect(state.stop).toHaveBeenCalledOnce();
    expect(screen.getByText("Saved connections unavailable")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Add VPN connection" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("uses the connected server and username instead of edited saved settings", () => {
    const state = model({
      status: {
        endpoint: "socks5h://127.0.0.1:49152", container_name: "test-vpn", connection_id: connection.connection_id,
        vpn_url: "https://connected.example.test", username: "connected-user", running: true, state: "connected",
      },
    });
    render(<VpnSidebar model={state} />);
    const current = within(screen.getByRole("region", { name: "Current VPN" }));
    expect(current.getByText("https://connected.example.test")).toBeTruthy();
    expect(current.getByText("connected-user")).toBeTruthy();
    expect(current.queryByText(connection.url)).toBeNull();
    expect(current.queryByText(connection.username)).toBeNull();
  });

  it("uses a matching profile when an older daemon omits connection identity", () => {
    const state = model({
      status: { endpoint: "socks5h://127.0.0.1:49152", container_name: "test-vpn", connection_id: connection.connection_id, running: true, state: "connected" },
    });
    render(<VpnSidebar model={state} />);
    const current = within(screen.getByRole("region", { name: "Current VPN" }));
    expect(current.getByText(connection.url)).toBeTruthy();
    expect(current.getByText(connection.username)).toBeTruthy();
  });

  it("omits URL credentials, paths, and query tokens from server summaries", () => {
    const state = model({
      connections: [{ ...connection, url: "vpn.example.test/private-group?token=example-token#example-fragment" }],
      status: {
        endpoint: "socks5h://127.0.0.1:49152", container_name: "test-vpn", connection_id: connection.connection_id,
        vpn_url: "https://url-user:url-password@connected.example.test/private-path?token=other-token",
        username: "connected-user", running: true, state: "connected",
      },
    });
    const { container, rerender } = render(<VpnSidebar model={state} />);
    const current = within(screen.getByRole("region", { name: "Current VPN" }));
    expect(current.getByText("https://connected.example.test")).toBeTruthy();
    expect(screen.getByText("https://vpn.example.test")).toBeTruthy();
    for (const hidden of ["url-user", "url-password", "private-path", "other-token", "private-group", "example-token", "example-fragment"]) {
      expect(container.textContent).not.toContain(hidden);
    }
    rerender(<VpnSidebar model={{ ...state, status: { ...state.status, vpn_url: undefined } }} />);
    expect(current.getByText("https://vpn.example.test")).toBeTruthy();
    expect(container.textContent).not.toContain("example-token");
  });

  it("keeps unavailable identity explicit for an external VPN from an older daemon", () => {
    const state = model({
      status: { endpoint: "socks5h://127.0.0.1:49152", container_name: "test-vpn", connection_id: null, running: true, state: "connected" },
    });
    render(<VpnSidebar model={state} />);
    const current = within(screen.getByRole("region", { name: "Current VPN" }));
    expect(current.getAllByText("Unavailable")).toHaveLength(2);
    expect(current.queryByText(connection.url)).toBeNull();
    expect(current.queryByText(connection.username)).toBeNull();
    expect((screen.getByRole("button", { name: "Connect Work" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("shows checking details before the first status response", () => {
    render(<VpnSidebar model={model({ status_loaded: false, status_loading: true })} />);
    const current = within(screen.getByRole("region", { name: "Current VPN" }));
    expect(current.getByText("Checking VPN…")).toBeTruthy();
    expect(current.getAllByText("Checking…")).toHaveLength(3);
    expect(current.queryByText("Disconnected")).toBeNull();
  });

  it("protects an active profile and allows canceling a pending connection", async () => {
    const user = userEvent.setup();
    const state = model({
      action: { kind: "connect", connection_id: "work" },
      status: { endpoint: null, container_name: null, connection_id: "work", running: false, state: "starting" },
    });
    render(<VpnSidebar model={state} />);
    expect((screen.getByRole("button", { name: "Edit Work" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Delete Work" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(screen.getByRole("button", { name: "Cancel connection to Work" }));
    expect(state.stop).toHaveBeenCalledOnce();
  });

  it("keeps the editor outside an inert workspace and reports stale runtime status", () => {
    const state = model({
      editor: { editor_id: 1, connection, expected_revision: "revision-1" },
      status_stale: true, status_error: "Unable to refresh",
    });
    render(<div inert><VpnSidebar model={state} /></div>);
    expect(screen.getByRole("dialog").closest("[inert]")).toBeNull();
    expect(screen.getByText("Status unavailable")).toBeTruthy();
    expect(screen.getByText("Unable to refresh")).toBeTruthy();
  });
});
