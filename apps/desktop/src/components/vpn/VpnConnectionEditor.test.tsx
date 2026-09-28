// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VpnConnection } from "../../lib/types";
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
      connection_id: "work", name: "Work", url: "https://vpn.example.test", username: "example-user",
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
