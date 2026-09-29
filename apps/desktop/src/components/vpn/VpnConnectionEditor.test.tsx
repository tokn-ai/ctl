// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { beginVpnEnrollment, cancelVpnEnrollment, openVpnSignIn, vpnEnrollmentStatus } from "../../lib/tauri";
import type { VpnConnection, VpnEnrollmentSnapshot } from "../../lib/types";
import { VpnConnectionEditor } from "./VpnConnectionEditor";

const connection: VpnConnection = {
  connection_id: "work", name: "Work", url: "https://vpn.example.test", username: "example-user",
  has_password: true, auth_method: "example-method", target_ip: "192.0.2.10",
};
afterEach(cleanup);

describe("VPN connection editor", () => {
  it("saves entered settings and clears the masked draft before the request finishes", async () => {
    const user = userEvent.setup();
    let finish!: (saved: boolean) => void;
    const on_save = vi.fn(() => new Promise<boolean>((resolve) => { finish = resolve; }));
    render(<VpnConnectionEditor connection={null} saving={false} error={null} on_save={on_save} on_close={vi.fn()} />);
    await user.type(screen.getByLabelText("Name"), "Work");
    await user.type(screen.getByLabelText("VPN server"), "vpn.example.test");
    await user.type(screen.getByLabelText("Username"), "example-user");
    const password = screen.getByLabelText("Password") as HTMLInputElement;
    expect(password.type).toBe("password");
    await user.type(password, "example-password");
    await user.click(screen.getByRole("button", { name: "Save connection" }));
    expect(on_save).toHaveBeenCalledWith(expect.objectContaining({
      name: "Work", url: "vpn.example.test", username: "example-user", password: "example-password",
      auth_method: null, target_ip: null,
    }));
    expect(password.value).toBe("");
    finish(true);
    await waitFor(() => expect(on_save).toHaveBeenCalledOnce());
  });

  it("edits summaries without fetching a secret and preserves a blank existing password", async () => {
    const user = userEvent.setup();
    const on_save = vi.fn().mockResolvedValue(true);
    render(<VpnConnectionEditor connection={connection} saving={false} error={null} on_save={on_save} on_close={vi.fn()} />);
    expect((screen.getByLabelText("Password") as HTMLInputElement).value).toBe("");
    await user.click(screen.getByRole("button", { name: "Save connection" }));
    expect(on_save).toHaveBeenCalledWith({
      provider: "openconnect", connection_id: "work", name: "Work", url: "https://vpn.example.test", username: "example-user",
      password: null, auth_method: "example-method", target_ip: "192.0.2.10",
    });
  });

  it("requires a new password and validates an optional connectivity address", async () => {
    const user = userEvent.setup();
    const on_save = vi.fn().mockResolvedValue(true);
    render(<VpnConnectionEditor connection={{ ...connection, has_password: false }} saving={false} error={null} on_save={on_save} on_close={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Save connection" }));
    expect(screen.getByRole("alert").textContent).toBe("Enter your password.");
    await user.type(screen.getByLabelText("Password"), "example-password");
    fireEvent.change(screen.getByLabelText("Connectivity check target"), { target: { value: "192.0.2.999" } });
    await user.click(screen.getByRole("button", { name: "Save connection" }));
    expect(screen.getByRole("alert").textContent).toContain("valid IPv4");
    expect(on_save).not.toHaveBeenCalled();
  });

  it("clears a canceled draft and requires re-entry after a failed password update", async () => {
    const user = userEvent.setup();
    const on_close = vi.fn();
    const on_save = vi.fn().mockResolvedValue(false);
    render(<VpnConnectionEditor connection={connection} saving={false} error="Unable to save" on_save={on_save} on_close={on_close} />);
    const password = screen.getByLabelText("Password") as HTMLInputElement;
    await user.type(password, "replacement-example");
    await user.click(screen.getByRole("button", { name: "Save connection" }));
    await screen.findByText("Re-enter the password before retrying the save.");
    expect(password.value).toBe("");
    await user.click(screen.getByRole("button", { name: "Save connection" }));
    expect(on_save).toHaveBeenCalledOnce();
    expect(screen.getByRole("alert").textContent).toBe("Enter your password.");
    await user.type(password, "draft-to-discard");
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(password.value).toBe("");
    expect(on_close).toHaveBeenCalledOnce();
  });
});


vi.mock("../../lib/tauri", () => ({
  beginVpnEnrollment: vi.fn(), cancelVpnEnrollment: vi.fn(), openVpnSignIn: vi.fn(), vpnEnrollmentStatus: vi.fn(),
}));
const enrollment: VpnEnrollmentSnapshot = {
  enrollment_id: "draft-one", connection_id: "native-generated-id", error: null,
  status: { provider: "tailscale", vpn_id: "native-generated-id", connection_id: "native-generated-id", state: "starting", running: false,
    endpoint: null, container_name: null, auth_url: "https://login.tailscale.com/a/example" },
};
beforeEach(() => {
  vi.mocked(beginVpnEnrollment).mockReset().mockResolvedValue(enrollment);
  vi.mocked(vpnEnrollmentStatus).mockReset().mockResolvedValue(enrollment);
  vi.mocked(openVpnSignIn).mockReset().mockResolvedValue(undefined);
  vi.mocked(cancelVpnEnrollment).mockReset().mockResolvedValue(undefined);
});

describe("Tailscale connection editor", () => {
  it("signs in before saving, freezes settings, and confirms the authenticated account", async () => {
    const user = userEvent.setup();
    const on_save = vi.fn();
    const on_save_enrollment = vi.fn().mockResolvedValue(true);
    render(<VpnConnectionEditor connection={null} saving={false} error={null} on_save={on_save} on_close={vi.fn()} enrollment_supported on_save_enrollment={on_save_enrollment} />);
    expect((screen.getByRole("combobox", { name: "Provider" }) as HTMLSelectElement).value).toBe("openconnect");
    await user.type(screen.getByLabelText("Password"), "discarded-example");
    await user.selectOptions(screen.getByRole("combobox", { name: "Provider" }), "tailscale");
    expect(screen.queryByLabelText("Password")).toBeNull();
    expect(screen.queryByLabelText("VPN server")).toBeNull();
    expect(screen.queryByLabelText("Device name in Tailscale")).toBeNull();
    await user.type(screen.getByLabelText("Name"), "Tailnet");
    await user.click(screen.getByRole("button", { name: "Advanced options" }));
    expect((screen.getByLabelText("Device name in Tailscale") as HTMLInputElement).placeholder).toBe("Assigned automatically");
    await user.type(screen.getByLabelText("Device name in Tailscale"), "rmux-work");
    expect((screen.getByRole("checkbox", { name: /Accept subnet routes/ }) as HTMLInputElement).checked).toBe(false);
    await user.click(screen.getByRole("checkbox", { name: /Accept subnet routes/ }));
    await user.click(screen.getByRole("button", { name: "Sign in with Tailscale" }));
    expect(beginVpnEnrollment).toHaveBeenCalledExactlyOnceWith({ name: "Tailnet", hostname: "rmux-work", accept_routes: true });
    expect(openVpnSignIn).toHaveBeenCalledExactlyOnceWith("native-generated-id");
    expect(on_save).not.toHaveBeenCalled();
    expect(on_save_enrollment).not.toHaveBeenCalled();
    expect((screen.getByLabelText("Name") as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByLabelText("Device name in Tailscale") as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByRole("button", { name: "Open browser" })).toBeTruthy();
    vi.mocked(vpnEnrollmentStatus).mockResolvedValue({ ...enrollment, status: { ...enrollment.status, state: "connected", running: true,
      auth_url: null, endpoint: "socks5h://127.0.0.1:49152", username: "sample@example.test", tailnet: "example.test", hostname: "rmux-work" } });
    fireEvent(window, new Event("focus"));
    expect(await screen.findByText("Signed in to Tailscale")).toBeTruthy();
    expect(screen.getByText("sample@example.test")).toBeTruthy();
    expect(screen.getByText("example.test")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Save connection" }));
    expect(on_save_enrollment).toHaveBeenCalledExactlyOnceWith("draft-one");
    expect(on_save).not.toHaveBeenCalled();
  });

  it("keeps existing Tailscale settings editable without a new enrollment", async () => {
    const user = userEvent.setup();
    const on_save = vi.fn().mockResolvedValue(true);
    const tailscale: VpnConnection = { provider: "tailscale", connection_id: "tailnet", name: "Tailnet", hostname: null, accept_routes: false };
    render(<VpnConnectionEditor connection={tailscale} saving={false} error={null} on_save={on_save} on_close={vi.fn()} />);
    expect((screen.getByRole("combobox", { name: /Provider/ }) as HTMLSelectElement).disabled).toBe(true);
    await user.click(screen.getByRole("button", { name: "Save connection" }));
    expect(on_save).toHaveBeenCalledWith(tailscale);
    expect(beginVpnEnrollment).not.toHaveBeenCalled();
  });

  it("rejects invalid device names before starting and explains the field", async () => {
    const user = userEvent.setup();
    render(<VpnConnectionEditor connection={null} saving={false} error={null} on_save={vi.fn()} on_close={vi.fn()} enrollment_supported />);
    await user.selectOptions(screen.getByRole("combobox", { name: "Provider" }), "tailscale");
    await user.type(screen.getByLabelText("Name"), "Tailnet");
    await user.click(screen.getByRole("button", { name: "Advanced options" }));
    expect(screen.getByText(/Leave blank for rmux to choose one/)).toBeTruthy();
    await user.type(screen.getByLabelText("Device name in Tailscale"), "invalid host");
    await user.click(screen.getByRole("button", { name: "Sign in with Tailscale" }));
    expect(screen.getByRole("alert").textContent).toContain("device name");
    expect(beginVpnEnrollment).not.toHaveBeenCalled();
  });

  it("explains unsupported daemons before a new enrollment", async () => {
    const user = userEvent.setup();
    render(<VpnConnectionEditor connection={null} saving={false} error={null} on_save={vi.fn()} on_close={vi.fn()} />);
    await user.selectOptions(screen.getByRole("combobox", { name: "Provider" }), "tailscale");
    expect(screen.getByRole("alert").textContent).toContain("Update and restart ctld");
    expect((screen.getByRole("button", { name: "Sign in with Tailscale" }) as HTMLButtonElement).disabled).toBe(true);
  });
});
