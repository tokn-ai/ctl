// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CredentialRecord, CredentialsSnapshot, CredentialTarget } from "../lib/types";
import { CredentialsPage } from "./CredentialsPage";

const api = vi.hoisted(() => ({ list: vi.fn(), forget: vi.fn(), identities: vi.fn(), save_identity: vi.fn(), forget_identity: vi.fn(), import_metadata: vi.fn() }));
vi.mock("../lib/tauri", () => ({ listSavedCredentials: api.list, forgetSavedCredential: api.forget, listIdentityFiles: api.identities, saveIdentityPassphrase: api.save_identity, forgetIdentityPassphrase: api.forget_identity, importCredentialMetadata: api.import_metadata }));

const credential: CredentialRecord = { credential_id: "opaque-keychain-id", name: "Development", kind: "ssh_password", storage: "keychain", target: "dev.example.test", account: "developer", created_at_ms: 1000, updated_at_ms: 2000, detail: "Requires Touch ID when used.", action: "forget", vpn_connection_id: null };
const vpn: CredentialRecord = { ...credential, credential_id: "vpn-id", name: "Work VPN", kind: "vpn_password", storage: "vpn_settings", target: "vpn.example.test", created_at_ms: null, updated_at_ms: null, detail: "Saved in private VPN settings.", action: "manage_vpn", vpn_connection_id: "work-vpn" };
const snapshot: CredentialsSnapshot = { metadata_import_required: false, credentials: [credential, vpn], sources: [{ source: "keychain", state: "ready", message: null }, { source: "vpn", state: "ready", message: null }], checked_at_ms: 3000 };
const targets: CredentialTarget[] = [{ name: "Development", target: { kind: "ssh", host_id: "development", destination: "dev.example.test" } }];
const props = () => ({ visible: true, targets, on_close: vi.fn(), on_dialog_change: vi.fn(), on_manage_vpn: vi.fn() });

beforeEach(() => {
  vi.clearAllMocks();
  api.list.mockResolvedValue(structuredClone(snapshot));
  api.forget.mockResolvedValue(undefined);
  api.import_metadata.mockResolvedValue(undefined);
  api.identities.mockResolvedValue({ metadata_import_required: false, keychain_message: null, identity_files: [], complete: true, warning: null, keychain_available: true, checked_at_ms: 3000 });
});
afterEach(cleanup);

describe("Credentials page", () => {
  it("lists only explicit metadata, with no secret values or reveal controls", async () => {
    const value = structuredClone(snapshot);
    Object.assign(value.credentials[0], { password: "hidden-password-fixture", passphrase: "hidden-passphrase-fixture", secret: "hidden-secret-fixture" });
    value.credentials.push({ ...credential, credential_id: "passphrase-id", name: "Development key", kind: "ssh_key_passphrase" });
    value.credentials.push({ ...credential, credential_id: "legacy-id", name: "Older SSH item", kind: "ssh_credential", target: null, account: null, updated_at_ms: null });
    value.credentials.push({ ...vpn, credential_id: "tailnet-id", name: "Team network", kind: "tailscale_sign_in", storage: "container_volume", detail: "Sign-in data has not been verified." });
    api.list.mockResolvedValue(value);
    const page = render(<CredentialsPage {...props()} />);
    expect(await screen.findByRole("table")).toBeTruthy();
    expect(api.list).toHaveBeenCalledWith(targets);
    for (const label of ["SSH password", "SSH key passphrase", "Saved SSH credential", "VPN password", "Tailscale sign-in", "VPN settings", "Container volume"]) expect(screen.getByText(label)).toBeTruthy();
    for (const secret of ["hidden-password-fixture", "hidden-passphrase-fixture", "hidden-secret-fixture"]) expect(page.container.innerHTML).not.toContain(secret);
    expect(page.container.querySelector("input[type=password]")).toBeNull();
    expect(screen.queryByRole("button", { name: /reveal|copy|show password/i })).toBeNull();
    expect(screen.getByText(/sign-in data has not been verified/i)).toBeTruthy();
    expect(screen.getAllByText("Not recorded").length).toBeGreaterThan(0);
  });

  it("loads only when opened and calls the close and VPN navigation callbacks", async () => {
    const options = props();
    const page = render(<CredentialsPage {...options} visible={false} />);
    expect(api.list).not.toHaveBeenCalled();
    page.rerender(<CredentialsPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Manage Work VPN" }));
    expect(options.on_manage_vpn).toHaveBeenCalledWith("work-vpn");
    expect(api.forget).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Back to workspace" }));
    expect(options.on_close).toHaveBeenCalledOnce();
  });

  it("requires confirmation and sends only the opaque ID to forget one Keychain item", async () => {
    const options = props();
    api.list.mockResolvedValueOnce(structuredClone(snapshot)).mockResolvedValueOnce({ ...snapshot, credentials: [vpn] });
    render(<CredentialsPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Forget Development" }));
    const dialog = await screen.findByRole("dialog", { name: "Forget Development" });
    expect(within(dialog).getByText(/Active connections stay connected/)).toBeTruthy();
    expect(api.forget).not.toHaveBeenCalled();
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(true);
    fireEvent.click(within(dialog).getByRole("button", { name: "Forget credential" }));
    await waitFor(() => expect(api.forget).toHaveBeenCalledExactlyOnceWith("opaque-keychain-id"));
    await waitFor(() => expect(api.list).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("Forgot Development. Active connections are unchanged.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Forget Development" })).toBeNull();
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(false);
  });

  it("cancels without deleting and discards unconfirmed deletion when hidden", async () => {
    const options = props();
    const page = render(<CredentialsPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Forget Development" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Forget Development" }));
    page.rerender(<CredentialsPage {...options} visible={false} />);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.forget).not.toHaveBeenCalled();
  });

  it("preserves other sources when Keychain cannot be listed", async () => {
    api.list.mockResolvedValue({ ...snapshot, credentials: [vpn], sources: [{ source: "keychain", state: "unavailable", message: "Keychain is locked. Unlock it, then refresh." }, snapshot.sources[1]] });
    render(<CredentialsPage {...props()} />);
    expect(await screen.findByText("Keychain: Keychain is locked. Unlock it, then refresh.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Manage Work VPN" })).toBeTruthy();
    expect(screen.queryByText("No saved credentials found.")).toBeNull();
  });

  it("distinguishes an empty inventory from unavailable storage", async () => {
    api.list.mockResolvedValue({ ...snapshot, credentials: [] });
    render(<CredentialsPage {...props()} />);
    expect(await screen.findByText("No saved credentials found.")).toBeTruthy();
    api.list.mockResolvedValue({ ...snapshot, credentials: [], sources: [{ source: "keychain", state: "unsupported", message: null }, snapshot.sources[1]] });
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByText("Keychain cannot be listed on this platform.")).toBeTruthy();
    expect(screen.getByText("No credentials found in available storage.")).toBeTruthy();
  });

  it("keeps the last inventory with an explicit stale message after refresh failure", async () => {
    render(<CredentialsPage {...props()} />);
    await screen.findByRole("table");
    api.list.mockRejectedValue(new Error("Inventory service unavailable."));
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Could not refresh credentials: Inventory service unavailable. Showing the last successful check.");
    expect(screen.getByRole("button", { name: "Forget Development" })).toBeTruthy();
  });

  it("does not claim the inventory is empty when every source failed", async () => {
    api.list.mockResolvedValue({ ...snapshot, credentials: [], sources: snapshot.sources.map((source) => ({ ...source, state: "unavailable" })) });
    render(<CredentialsPage {...props()} />);
    expect(await screen.findByText("Saved credentials could not be checked.")).toBeTruthy();
    expect(screen.queryByText(/No saved credentials found|No credentials found in available storage/)).toBeNull();
  });

  it("never offers Keychain deletion for a VPN record even with an inconsistent action", async () => {
    api.list.mockResolvedValue({ ...snapshot, credentials: [{ ...vpn, action: "forget" }] });
    const options = props();
    render(<CredentialsPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Manage Work VPN" }));
    expect(screen.queryByRole("button", { name: /Forget/ })).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.forget).not.toHaveBeenCalled();
    expect(options.on_manage_vpn).toHaveBeenCalledWith("work-vpn");
  });

  it("reports deletion failure without removing the credential or claiming success", async () => {
    api.forget.mockRejectedValue(new Error("Keychain access was denied."));
    render(<CredentialsPage {...props()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Forget Development" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Forget credential" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Could not forget Development: Keychain access was denied.");
    expect(screen.getByRole("button", { name: "Forget Development" })).toHaveProperty("disabled", false);
    expect(screen.queryByText(/Forgot Development/)).toBeNull();
  });

  it("ignores a late inventory from before reopening with a different target catalog", async () => {
    let complete!: (value: CredentialsSnapshot) => void;
    api.list.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = render(<CredentialsPage {...options} />);
    expect(screen.getByText("Checking saved credentials…")).toBeTruthy();
    page.rerender(<CredentialsPage {...options} visible={false} />);
    const next_targets = [{ ...targets[0], name: "Renamed development" }];
    page.rerender(<CredentialsPage {...options} targets={next_targets} />);
    await screen.findByRole("table");
    expect(api.list).toHaveBeenLastCalledWith(next_targets);
    await act(async () => complete({ ...snapshot, credentials: [{ ...credential, name: "Stale name" }] }));
    expect(screen.queryByText("Stale name")).toBeNull();
    expect(screen.getByRole("button", { name: "Forget Development" })).toBeTruthy();
  });

  it("does not restore a forgotten item from an older in-flight refresh", async () => {
    let complete!: (value: CredentialsSnapshot) => void;
    api.list.mockResolvedValueOnce(structuredClone(snapshot))
      .mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }))
      .mockResolvedValueOnce({ ...snapshot, credentials: [vpn] });
    render(<CredentialsPage {...props()} />);
    await screen.findByRole("table");
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    fireEvent.click(screen.getByRole("button", { name: "Forget Development" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Forget credential" }));
    await screen.findByText("Forgot Development. Active connections are unchanged.");
    await act(async () => complete(structuredClone(snapshot)));
    expect(screen.queryByRole("button", { name: "Forget Development" })).toBeNull();
  });
});
