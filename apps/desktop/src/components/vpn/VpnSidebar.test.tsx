// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VpnController } from "../../features/vpn/useVpn";
import type { VpnConnection, VpnStatus } from "../../lib/types";
import { VpnSidebar } from "./VpnSidebar";

const connection: VpnConnection = {
  connection_id: "work", name: "Work", url: "https://vpn.example.test", username: "example-user",
  has_password: true, auth_method: null, target_ip: null,
};
const research = { ...connection, connection_id: "research", name: "Research" };
function runtime(vpn_id = "work", overrides: Partial<VpnStatus> = {}): VpnStatus {
  return {
    vpn_id, connection_id: vpn_id, state: "connected", running: true,
    vpn_url: "https://gateway.example.test", username: "connected-user",
    endpoint: `socks5h://127.0.0.1:${vpn_id === "work" ? "49152" : "49153"}`, container_name: `sample-${vpn_id}`,
    ...overrides,
  };
}
function model(overrides: Partial<VpnController> = {}): VpnController {
  return {
    connections: [connection], catalog_loaded: true, catalog_loading: false, catalog_error: null,
    statuses: [], supports_multiple: true, supported_providers: ["openconnect", "tailscale"], supports_tailscale_enrollment: true, enrollment_connection_id: null, signing_in_ids: new Set(),
    status_loaded: true, status_loading: false, status_stale: false, status_error: null, discovery_warnings: [], last_checked_at: null,
    actions: new Map(), action_errors: new Map(), uncertain_ids: new Set(), profile_busy: false, deleting_id: null,
    editor: null, editor_error: null, editor_saving: false,
    refresh: vi.fn().mockResolvedValue(undefined), connect: vi.fn().mockResolvedValue(undefined), stop: vi.fn().mockResolvedValue(undefined),
    signIn: vi.fn().mockResolvedValue(undefined), addConnection: vi.fn(), editConnection: vi.fn(), closeEditor: vi.fn(),
    saveEnrollment: vi.fn().mockResolvedValue(true), setEnrollmentConnectionId: vi.fn(),
    saveConnection: vi.fn().mockResolvedValue(true), deleteConnection: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}
function region(name = "Work") { return within(screen.getByRole("region", { name })); }
afterEach(cleanup);

describe("VPN sidebar", () => {
  it("manages a saved connection without exposing a password or env control", async () => {
    const user = userEvent.setup();
    const state = model();
    render(<VpnSidebar model={state} />);
    await user.click(region().getByRole("button", { name: "Connect Work" }));
    await user.click(region().getByRole("button", { name: "Edit Work" }));
    await user.click(region().getByRole("button", { name: "Delete Work" }));
    expect(state.connect).toHaveBeenCalledWith("work");
    expect(state.editConnection).toHaveBeenCalledWith(connection);
    expect(state.deleteConnection).toHaveBeenCalledWith("work");
    expect(screen.queryByText(/env.file/i)).toBeNull();
    expect(screen.queryByLabelText("Password")).toBeNull();
  });

  it("keeps each saved VPN in one item through connect and disconnect", async () => {
    const user = userEvent.setup();
    const clipboard = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue(undefined);
    const state = model();
    const { rerender } = render(<VpnSidebar model={state} />);
    expect(region().getByText("Disconnected")).toBeTruthy();
    rerender(<VpnSidebar model={{ ...state, statuses: [runtime()] }} />);
    expect(screen.getAllByRole("region")).toHaveLength(1);
    expect(region().getByText("Connected")).toBeTruthy();
    expect(region().getByText("https://gateway.example.test")).toBeTruthy();
    expect(region().queryByText(connection.url)).toBeNull();
    await user.click(region().getByRole("button", { name: "Copy SOCKS endpoint" }));
    expect(clipboard).toHaveBeenCalledWith("socks5h://127.0.0.1:49152");
    await user.click(region().getByRole("button", { name: "Disconnect Work" }));
    expect(state.stop).toHaveBeenCalledWith("work");
    rerender(<VpnSidebar model={state} />);
    expect(screen.getAllByRole("region")).toHaveLength(1);
    expect(region().getByText("Disconnected")).toBeTruthy();
    expect(region().queryByText("socks5h://127.0.0.1:49152")).toBeNull();
  });

  it("allows another profile to connect and be edited while one is active", async () => {
    const user = userEvent.setup();
    const state = model({ connections: [connection, research], statuses: [runtime()] });
    render(<VpnSidebar model={state} />);
    expect((region().getByRole("button", { name: "Edit Work" }) as HTMLButtonElement).disabled).toBe(true);
    expect((region().getByRole("button", { name: "Delete Work" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(region("Research").getByRole("button", { name: "Connect Research" }));
    expect(state.connect).toHaveBeenCalledWith("research");
    await user.click(region("Research").getByRole("button", { name: "Edit Research" }));
    expect(state.editConnection).toHaveBeenCalledWith(research);
  });

  it("copies and disconnects each of two active saved VPNs independently", async () => {
    const user = userEvent.setup();
    const clipboard = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue(undefined);
    const state = model({ connections: [connection, research], statuses: [runtime(), runtime("research")] });
    const { container } = render(<VpnSidebar model={state} />);
    expect(container.querySelector(".vpn-count")?.textContent).toBe("2");
    await user.click(region("Research").getByRole("button", { name: "Copy SOCKS endpoint" }));
    expect(clipboard).toHaveBeenLastCalledWith("socks5h://127.0.0.1:49153");
    await user.click(region("Research").getByRole("button", { name: "Disconnect Research" }));
    expect(state.stop).toHaveBeenCalledExactlyOnceWith("research");
    expect(region().getByText("Connected")).toBeTruthy();
  });

  it("renders one synthetic item per unmatched runtime and never matches by server or user", () => {
    const state = model({ statuses: [runtime("cli-a", { connection_id: null, vpn_url: connection.url, username: connection.username }), runtime("cli-b", { connection_id: "unknown" })] });
    const { container, rerender } = render(<VpnSidebar model={state} />);
    expect(screen.getAllByRole("region")).toHaveLength(3);
    expect(container.querySelector(".vpn-count")?.textContent).toBe("3");
    expect(region().getByText("Disconnected")).toBeTruthy();
    expect(screen.getAllByText("Not saved in this app")).toHaveLength(2);
    for (const item of screen.getAllByRole("region", { name: "Connected VPN" })) {
      expect(within(item).queryByRole("button", { name: /Edit|Delete/ })).toBeNull();
    }
    rerender(<VpnSidebar model={model()} />);
    expect(screen.getAllByRole("region")).toHaveLength(1);
  });

  it("keeps CLI runtimes manageable when the saved catalog cannot load", async () => {
    const user = userEvent.setup();
    const state = model({ connections: [], catalog_loaded: false, catalog_error: "Catalog unavailable", statuses: [runtime("cli", { connection_id: null })] });
    render(<VpnSidebar model={state} />);
    expect(region("Connected VPN").getByText("Saved connection unavailable")).toBeTruthy();
    await user.click(region("Connected VPN").getByRole("button", { name: "Disconnect VPN" }));
    expect(state.stop).toHaveBeenCalledWith("cli");
    expect(screen.getByText("Catalog unavailable")).toBeTruthy();
  });

  it("merges an unmatched runtime into its saved item when the catalog arrives", () => {
    const state = model({ connections: [], catalog_loaded: false, catalog_loading: true, statuses: [runtime()] });
    const { rerender } = render(<VpnSidebar model={state} />);
    expect(region("Connected VPN").getByText("Loading saved connections…")).toBeTruthy();
    expect(screen.queryByText("Not saved in this app")).toBeNull();
    rerender(<VpnSidebar model={{ ...state, catalog_loaded: true, catalog_loading: false, connections: [connection] }} />);
    expect(screen.getAllByRole("region")).toHaveLength(1);
    expect(region().getByText("Connected")).toBeTruthy();
  });

  it("cancels the selected pending connection without blocking another saved profile", async () => {
    const user = userEvent.setup();
    const state = model({
      connections: [connection, research],
      actions: new Map([["work", { kind: "connect", connection_id: "work" }]]),
      statuses: [runtime("work", { state: "starting", running: false, endpoint: null })],
    });
    render(<VpnSidebar model={state} />);
    expect(region().getByText("Connecting…")).toBeTruthy();
    expect(region().getByText("Pending")).toBeTruthy();
    await user.click(region().getByRole("button", { name: "Cancel connection to Work" }));
    expect(state.stop).toHaveBeenCalledWith("work");
    expect((region("Research").getByRole("button", { name: "Connect Research" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("supports action identity before its optimistic runtime render", async () => {
    const user = userEvent.setup();
    const state = model({ actions: new Map([["work", { kind: "connect", connection_id: "work" }]]) });
    render(<VpnSidebar model={state} />);
    expect(screen.getAllByRole("region")).toHaveLength(1);
    expect(region().getByText("Connecting…")).toBeTruthy();
    await user.click(region().getByRole("button", { name: "Cancel connection to Work" }));
    expect(state.stop).toHaveBeenCalledWith("work");
  });

  it("keeps older-daemon runtimes visible but disables unsafe targeted stop and additional connects", async () => {
    const user = userEvent.setup();
    const state = model({ supports_multiple: false, statuses: [runtime("legacy", { connection_id: null })] });
    render(<VpnSidebar model={state} />);
    const stop = region("Connected VPN").getByRole("button", { name: "Disconnect VPN" }) as HTMLButtonElement;
    expect(stop.disabled).toBe(true);
    expect(stop.title).toContain("Update ctld");
    expect((region().getByRole("button", { name: "Connect Work" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(stop);
    expect(state.stop).not.toHaveBeenCalled();
    expect(screen.getByText(/Update ctld to manage individual VPNs/)).toBeTruthy();
    expect(region("Connected VPN").getByText("socks5h://127.0.0.1:49153")).toBeTruthy();
  });

  it("keeps errors and uncertainty on the affected item", async () => {
    const user = userEvent.setup();
    const state = model({
      connections: [connection, research], uncertain_ids: new Set(["work"]),
      action_errors: new Map([["work", "Request timed out"]]),
    });
    render(<VpnSidebar model={state} />);
    expect(region().getByText("Checking…")).toBeTruthy();
    expect(region().getByRole("alert").textContent).toBe("Request timed out");
    expect(region("Research").queryByRole("alert")).toBeNull();
    expect((region().getByRole("button", { name: "Edit Work" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(region().getByRole("button", { name: "Disconnect Work" }));
    expect(state.stop).toHaveBeenCalledWith("work");
    expect((region("Research").getByRole("button", { name: "Connect Research" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("retains stale active details and targeted recovery, but no untargeted recovery", () => {
    const { rerender } = render(<VpnSidebar model={model({ statuses: [runtime()], status_stale: true })} />);
    expect(region().getByText("Status unavailable")).toBeTruthy();
    expect(region().getByText("Last known state: Connected")).toBeTruthy();
    expect((region().getByRole("button", { name: "Copy SOCKS endpoint" }) as HTMLButtonElement).disabled).toBe(true);
    expect((region().getByRole("button", { name: "Disconnect Work" }) as HTMLButtonElement).disabled).toBe(false);
    rerender(<VpnSidebar model={model({ connections: [], status_stale: true })} />);
    expect(screen.queryByRole("region")).toBeNull();
    expect(screen.queryByRole("button", { name: /Disconnect/ })).toBeNull();
  });

  it("prefers runtime identity and strips URL credentials, paths, and tokens", () => {
    const state = model({
      connections: [{ ...connection, url: "vpn.example.test/private-group?token=example-token" }],
      statuses: [runtime("work", { vpn_url: "https://url-user:url-password@gateway.example.test/private-path?token=other-token" })],
    });
    const { container, rerender } = render(<VpnSidebar model={state} />);
    expect(region().getByText("https://gateway.example.test")).toBeTruthy();
    for (const hidden of ["url-user", "url-password", "private-path", "other-token", "private-group", "example-token"]) expect(container.textContent).not.toContain(hidden);
    rerender(<VpnSidebar model={{ ...state, statuses: [runtime("work", { vpn_url: undefined, username: undefined })] }} />);
    expect(region().getByText("https://vpn.example.test")).toBeTruthy();
    expect(region().getByText(connection.username)).toBeTruthy();
  });

  it("reports copy failures only on the endpoint's item", async () => {
    const user = userEvent.setup();
    vi.spyOn(navigator.clipboard, "writeText").mockRejectedValueOnce(new Error("Clipboard unavailable"));
    render(<VpnSidebar model={model({ connections: [connection, research], statuses: [runtime(), runtime("research")] })} />);
    await user.click(region("Research").getByRole("button", { name: "Copy SOCKS endpoint" }));
    expect(region("Research").getByRole("alert").textContent).toBe("Clipboard unavailable");
    expect(region().queryByRole("alert")).toBeNull();
  });

  it("keeps the editor outside the inert workspace", () => {
    render(<div inert><VpnSidebar model={model({ editor: { editor_id: 1, connection, expected_revision: "revision-1" } })} /></div>);
    expect(screen.getByRole("dialog").closest("[inert]")).toBeNull();
  });
});


describe("Tailscale VPN items", () => {
  const tailscale: VpnConnection = { provider: "tailscale", connection_id: "tailnet", name: "Tailnet", hostname: "rmux-work", accept_routes: false };
  const pending = runtime("tailnet", { provider: "tailscale", state: "starting", running: false, endpoint: null,
    auth_url: "https://login.tailscale.com/a/example", vpn_url: null, username: null });

  it("connects without a password and disables unsupported providers on an old owner", async () => {
    const user = userEvent.setup();
    const state = model({ connections: [tailscale] });
    const { rerender } = render(<VpnSidebar model={state} />);
    await user.click(region("Tailnet").getByRole("button", { name: "Connect Tailnet" }));
    expect(state.connect).toHaveBeenCalledWith("tailnet");
    expect(region("Tailnet").getByText("Tailscale")).toBeTruthy();
    expect(region("Tailnet").queryByText("VPN server")).toBeNull();
    rerender(<VpnSidebar model={{ ...state, supported_providers: ["openconnect"] }} />);
    expect((region("Tailnet").getByRole("button", { name: "Connect Tailnet" }) as HTMLButtonElement).disabled).toBe(true);
    expect(region("Tailnet").getByText("Update ctld to connect with Tailscale.")).toBeTruthy();
  });

  it("opens native sign-in by runtime ID and keeps cancellation available", async () => {
    const user = userEvent.setup();
    const state = model({ connections: [tailscale], statuses: [pending] });
    const { rerender } = render(<VpnSidebar model={state} />);
    expect(region("Tailnet").getByText("Sign-in required")).toBeTruthy();
    expect(screen.queryByRole("link")).toBeNull();
    expect(screen.queryByText(pending.auth_url!)).toBeNull();
    await user.click(region("Tailnet").getByRole("button", { name: "Sign in to Tailnet" }));
    expect(state.signIn).toHaveBeenCalledWith("tailnet");
    rerender(<VpnSidebar model={{ ...state, signing_in_ids: new Set(["tailnet"]) }} />);
    expect((region("Tailnet").getByRole("button", { name: "Sign in to Tailnet" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(region("Tailnet").getByRole("button", { name: "Cancel connection to Tailnet" }));
    expect(state.stop).toHaveBeenCalledWith("tailnet");
    rerender(<VpnSidebar model={{ ...state, statuses: [{ ...pending, state: "connected", auth_url: null, hostname: "rmux-node", tailnet: "example.test", username: "user@example.test" }] }} />);
    expect(region("Tailnet").getByText("Connected")).toBeTruthy();
    expect(region("Tailnet").getByText("rmux-node")).toBeTruthy();
    expect(region("Tailnet").getByText("example.test")).toBeTruthy();
    expect(region("Tailnet").queryByRole("button", { name: /Sign in/ })).toBeNull();
  });

  it("supports an external pending runtime and prevents sign-in from stale status", async () => {
    const user = userEvent.setup();
    const state = model({ connections: [], statuses: [{ ...pending, vpn_id: "cli-tailnet", connection_id: null }] });
    const { rerender } = render(<VpnSidebar model={state} />);
    await user.click(screen.getByRole("button", { name: "Sign in to Tailscale" }));
    expect(state.signIn).toHaveBeenCalledWith("cli-tailnet");
    rerender(<VpnSidebar model={{ ...state, status_stale: true }} />);
    expect((screen.getByRole("button", { name: "Sign in to Tailscale" }) as HTMLButtonElement).disabled).toBe(true);
  });
});


it("hides only the current enrollment from synthetic runtime items", () => {
  render(<VpnSidebar model={model({ connections: [], enrollment_connection_id: "draft-one", statuses: [
    runtime("draft-one", { provider: "tailscale", state: "starting" }), runtime("external", { provider: "tailscale" }),
  ] })} />);
  expect(screen.getAllByRole("region")).toHaveLength(1);
  expect(screen.getByText("Not saved in this app")).toBeTruthy();
});


describe("shared VPN container items", () => {
  it("shows a shared saved connection as connected and can join it but cannot release another daemon's interest", async () => {
    const user = userEvent.setup();
    const state = model({ statuses: [runtime("work", { shared_container: true, locally_connected: false })] });
    const { rerender } = render(<VpnSidebar model={state} />);
    expect(region().getByText("Connected")).toBeTruthy();
    expect(region().getByText("Shared VPN · This ctld is not keeping it connected.")).toBeTruthy();
    expect(region().getByText("socks5h://127.0.0.1:49152")).toBeTruthy();
    expect((region().getByRole("button", { name: "Disconnect Work" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(region().getByRole("button", { name: "Connect Work" }));
    expect(state.connect).toHaveBeenCalledWith("work");
    expect((region().getByRole("button", { name: "Edit Work" }) as HTMLButtonElement).disabled).toBe(true);
    expect((region().getByRole("button", { name: "Delete Work" }) as HTMLButtonElement).disabled).toBe(true);
    rerender(<VpnSidebar model={{ ...state, statuses: [runtime("work", { shared_container: true, locally_connected: true })] }} />);
    expect(region().queryByRole("button", { name: "Connect Work" })).toBeNull();
    await user.click(region().getByRole("button", { name: "Disconnect Work" }));
    expect(state.stop).toHaveBeenCalledWith("work");
  });

  it("shows unknown shared profiles without offering a configuration-free Connect or Disconnect", () => {
    render(<VpnSidebar model={model({ connections: [], statuses: [runtime("external", { connection_id: null, shared_container: true, locally_connected: false })] })} />);
    expect(region("Connected VPN").getByText("Connected")).toBeTruthy();
    expect(region("Connected VPN").queryByRole("button", { name: /Connect / })).toBeNull();
    expect((region("Connected VPN").getByRole("button", { name: "Disconnect VPN" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("keeps confirmed local rows usable while warning about incomplete shared discovery", () => {
    const state = model({ connections: [connection, research], statuses: [runtime()], discovery_warnings: ["Container inventory unavailable"] });
    const { rerender } = render(<VpnSidebar model={state} />);
    expect(region().getByText("Connected")).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toBe("Container inventory unavailable");
    expect((region().getByRole("button", { name: "Disconnect Work" }) as HTMLButtonElement).disabled).toBe(false);
    expect((region("Research").getByRole("button", { name: "Connect Research" }) as HTMLButtonElement).disabled).toBe(false);
    rerender(<VpnSidebar model={{ ...state, connections: [], statuses: [] }} />);
    expect(screen.queryByText("No saved VPN connections.")).toBeNull();
    expect(screen.getByRole("status").textContent).toBe("VPN status unavailable.");
  });
});

it("shows retained released shared metadata as unavailable and disables copying until confirmed", () => {
  render(<VpnSidebar model={model({ statuses: [runtime("work", { shared_container: true, locally_connected: false, status_unavailable: true })] })} />);
  expect(region().getByText("Status unavailable")).toBeTruthy();
  expect(region().getByText("Last known state: Connected")).toBeTruthy();
  expect(region().getByText("socks5h://127.0.0.1:49152")).toBeTruthy();
  expect((region().getByRole("button", { name: "Copy SOCKS endpoint" }) as HTMLButtonElement).disabled).toBe(true);
  expect((region().getByRole("button", { name: "Disconnect Work" }) as HTMLButtonElement).disabled).toBe(true);
  expect((region().getByRole("button", { name: "Connect Work" }) as HTMLButtonElement).disabled).toBe(true);
});
