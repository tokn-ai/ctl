// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CredentialTarget, IdentityFile, IdentitySnapshot } from "../lib/types";
import { CredentialsPage } from "./CredentialsPage";

const api = vi.hoisted(() => ({ list: vi.fn(), forget: vi.fn(), identities: vi.fn(), save_identity: vi.fn(), forget_identity: vi.fn() }));
vi.mock("../lib/tauri", () => ({ listSavedCredentials: api.list, forgetSavedCredential: api.forget, listIdentityFiles: api.identities, saveIdentityPassphrase: api.save_identity, forgetIdentityPassphrase: api.forget_identity }));
const file: IdentityFile = { identity_id: "identity-fixture", path: "/Users/sample/.ssh/id_ed25519", display_path: "~/.ssh/id_ed25519", file_version: "fixture-version", key_type: "Ed25519", fingerprint: "SHA256:fixture-fingerprint", encrypted: true, file_state: "ready", passphrase_state: "not_saved", detail: null, used_by: ["Development", "Staging"] };
const inventory: IdentitySnapshot = { identity_files: [file], complete: true, warning: null, keychain_available: true, checked_at_ms: 3000 };
const targets: CredentialTarget[] = [{ name: "Development", target: { kind: "ssh", host_id: "development", destination: "dev.example.test", identity_file: "~/.ssh/id_ed25519" } }];
const props = () => ({ visible: true, targets, on_close: vi.fn(), on_dialog_change: vi.fn(), on_manage_vpn: vi.fn() });
const makeInventory = (rows: IdentityFile[]): IdentitySnapshot => ({ ...inventory, identity_files: rows });

beforeEach(() => {
  vi.resetAllMocks();
  api.list.mockResolvedValue({ credentials: [], sources: [{ source: "keychain", state: "ready", message: null }, { source: "vpn", state: "ready", message: null }], checked_at_ms: 3000 });
  api.identities.mockResolvedValue(structuredClone(inventory));
  api.save_identity.mockResolvedValue(undefined);
  api.forget_identity.mockResolvedValue(undefined);
});
afterEach(cleanup);

async function openSave() {
  fireEvent.click(await screen.findByRole("button", { name: "Save passphrase for id_ed25519" }));
  const input = screen.getByLabelText("Key passphrase") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "sample-secret-fixture" } });
  return input;
}

function submitSave() {
  fireEvent.click(screen.getByRole("button", { name: "Verify and save" }));
}

describe("Identity file credentials", () => {
  it("keeps unavailable host file hints separate from legacy scope matching", async () => {
    const unavailable_targets = [{ ...targets[0], target: { ...targets[0].target, unavailable: "Device unavailable" } }];
    render(<CredentialsPage {...props()} targets={unavailable_targets} />);
    await screen.findByRole("table", { name: "Identity files" });
    expect(api.identities).toHaveBeenCalledExactlyOnceWith(unavailable_targets);
    expect(api.list).toHaveBeenCalledExactlyOnceWith([]);
  });

  it("shows one row per file with verified metadata, references and storage status", async () => {
    render(<CredentialsPage {...props()} />);
    const table = await screen.findByRole("table", { name: "Identity files" });
    expect(within(table).getAllByRole("row")).toHaveLength(2);
    for (const label of ["id_ed25519", "~/.ssh/id_ed25519", "Ed25519", "SHA256:fixture-fingerprint", "Development, Staging", "Not saved"]) expect(within(table).getByText(label)).toBeTruthy();
    expect(api.identities).toHaveBeenCalledExactlyOnceWith(targets);
    expect(screen.queryByRole("button", { name: /reveal|copy|delete file/i })).toBeNull();
  });

  it("offers save only for readable encrypted files and forget for a saved missing file", async () => {
    api.identities.mockResolvedValue(makeInventory([
      { ...file, identity_id: "plain", path: "/plain", display_path: "/plain", encrypted: false, passphrase_state: "not_required" },
      { ...file, identity_id: "missing", path: "/missing", display_path: "/missing", file_state: "missing", file_version: null, encrypted: null, key_type: null, fingerprint: null, passphrase_state: "saved" },
      { ...file, identity_id: "bad", path: "/bad", display_path: "/bad", file_state: "unsupported", encrypted: null, passphrase_state: "unknown" },
    ]));
    render(<CredentialsPage {...props()} />);
    await screen.findByRole("table", { name: "Identity files" });
    expect(screen.queryByRole("button", { name: /Save passphrase|Replace passphrase/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Forget passphrase for missing" })).toBeTruthy();
    expect(screen.getByText("Not required")).toBeTruthy();
    expect(screen.getByText("File missing")).toBeTruthy();
  });

  it("sends the entered passphrase only for native verification, clears the input and refreshes both inventories", async () => {
    let complete!: () => void;
    api.save_identity.mockImplementationOnce(() => new Promise<void>((resolve) => { complete = resolve; }));
    api.identities.mockResolvedValueOnce(inventory).mockResolvedValueOnce(makeInventory([{ ...file, passphrase_state: "saved" }]));
    const options = props();
    render(<CredentialsPage {...options} />);
    const input = await openSave();
    expect(input.type).toBe("password");
    submitSave();
    expect(input.value).toBe("");
    expect(screen.queryByLabelText("Key passphrase")).toBeNull();
    expect(api.save_identity).toHaveBeenCalledExactlyOnceWith({ path: file.path, file_version: file.file_version, passphrase: "sample-secret-fixture" });
    expect(screen.getByText("Verifying the passphrase and saving to Keychain…")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel quick input" })).toHaveProperty("disabled", true);
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(true);
    expect(document.body.textContent).not.toContain("sample-secret-fixture");
    await act(async () => complete());
    expect(await screen.findByText("Saved")).toBeTruthy();
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(api.identities).toHaveBeenCalledTimes(2);
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(false);
  });

  it.each(["cancel", "hide", "unmount"])("clears a detached secret input on %s without saving", async (action) => {
    const options = props();
    const page = render(<CredentialsPage {...options} />);
    const input = await openSave();
    if (action === "cancel") fireEvent.click(screen.getByRole("button", { name: "Cancel quick input" }));
    if (action === "hide") page.rerender(<CredentialsPage {...options} visible={false} />);
    if (action === "unmount") page.unmount();
    expect(input.value).toBe("");
    expect(api.save_identity).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(false);
  });

  it("discards passphrases after rejection and never renders untrusted native error text", async () => {
    api.save_identity.mockRejectedValue(new Error("sample-secret-fixture was invalid"));
    render(<CredentialsPage {...props()} />);
    const input = await openSave();
    submitSave();
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "The passphrase could not be saved. Check the passphrase, identity file, and Keychain access, then try again.");
    expect(input.value).toBe("");
    expect(document.body.textContent).not.toContain("sample-secret-fixture");
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Save passphrase for id_ed25519" }));
    expect(screen.getByLabelText("Key passphrase")).toHaveProperty("value", "");
  });

  it.each([
    ["identity_file_changed", "The identity file changed. Refresh and try again with the current file."],
    ["identity_unlock_failed", "The passphrase does not unlock this identity file. Try again."],
    ["identity_keychain_locked", "Keychain access was denied or locked. Unlock it and try again."],
  ])("uses only allowlisted error codes for %s", async (code, message) => {
    api.save_identity.mockRejectedValue({ code, message: "sample-secret-fixture" });
    render(<CredentialsPage {...props()} />);
    await openSave();
    submitSave();
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", message);
    expect(document.body.textContent).not.toContain("sample-secret-fixture");
  });

  it("explains replacement when the saved passphrase belongs to an older file", async () => {
    api.identities.mockResolvedValue(makeInventory([{ ...file, passphrase_state: "file_changed" }]));
    render(<CredentialsPage {...props()} />);
    expect(await screen.findByText("File changed · saved for older file")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Replace passphrase for id_ed25519" }));
    expect(screen.getByText(/Verify a passphrase for the current file to replace/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Verify and replace" })).toBeTruthy();
    expect(api.save_identity).not.toHaveBeenCalled();
  });

  it("confirms and forgets only the saved passphrase, then refreshes both sources", async () => {
    api.identities.mockResolvedValueOnce(makeInventory([{ ...file, passphrase_state: "saved" }])).mockResolvedValueOnce(inventory);
    render(<CredentialsPage {...props()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Forget passphrase for id_ed25519" }));
    expect(screen.getByText(/The identity file .* stays in place/)).toBeTruthy();
    expect(api.forget_identity).not.toHaveBeenCalled();
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Forget passphrase" }));
    expect(await screen.findByText("Forgot the passphrase for id_ed25519. The identity file is unchanged.")).toBeTruthy();
    expect(api.forget_identity).toHaveBeenCalledExactlyOnceWith(file.identity_id);
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(api.forget).not.toHaveBeenCalled();
  });

  it("isolates identity discovery errors and disables unavailable Keychain actions", async () => {
    api.identities.mockResolvedValue({ ...inventory, complete: false, keychain_available: false, warning: "One configured file could not be inspected." });
    render(<CredentialsPage {...props()} />);
    expect(await screen.findByText("One configured file could not be inspected.")).toBeTruthy();
    expect(screen.getByText(/Keychain passphrase storage could not be checked/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Save passphrase|Forget passphrase/ })).toBeNull();
    expect(screen.getByText("Not checked")).toBeTruthy();
    expect(screen.getByText("No saved credentials found.")).toBeTruthy();
    api.identities.mockRejectedValue(new Error("Identity discovery unavailable."));
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByText(/Identity discovery unavailable. Showing the last successful check/)).toBeTruthy();
    expect(screen.getByRole("table", { name: "Identity files" })).toBeTruthy();
    expect(screen.getByText("No saved credentials found.")).toBeTruthy();
  });

  it("ignores a pre-mutation refresh that would restore a forgotten passphrase", async () => {
    let complete!: (value: IdentitySnapshot) => void;
    const saved = makeInventory([{ ...file, passphrase_state: "saved" }]);
    api.identities.mockResolvedValueOnce(saved).mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; })).mockResolvedValueOnce(inventory);
    render(<CredentialsPage {...props()} />);
    await screen.findByText("Saved");
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    fireEvent.click(screen.getByRole("button", { name: "Forget passphrase for id_ed25519" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Forget passphrase" }));
    await screen.findByText("Not saved");
    await act(async () => complete(saved));
    expect(screen.queryByText("Saved")).toBeNull();
    expect(screen.queryByRole("button", { name: "Forget passphrase for id_ed25519" })).toBeNull();
  });

  it("continues an in-flight save after hiding without falsely reporting cancellation or allowing another mutation", async () => {
    let complete!: () => void;
    api.save_identity.mockImplementationOnce(() => new Promise<void>((resolve) => { complete = resolve; }));
    const options = props();
    const page = render(<CredentialsPage {...options} />);
    const input = await openSave();
    submitSave();
    page.rerender(<CredentialsPage {...options} visible={false} />);
    expect(input.value).toBe("");
    expect(screen.queryByRole("dialog")).toBeNull();
    page.rerender(<CredentialsPage {...options} />);
    await waitFor(() => expect(api.identities).toHaveBeenCalledTimes(2));
    expect(screen.getByRole("button", { name: "Save passphrase for id_ed25519" })).toHaveProperty("disabled", true);
    expect(screen.queryByText(/cancelled/i)).toBeNull();
    await act(async () => complete());
    expect(await screen.findByText("Saved the passphrase for id_ed25519 in Keychain.")).toBeTruthy();
    expect(api.save_identity).toHaveBeenCalledOnce();
  });
});
