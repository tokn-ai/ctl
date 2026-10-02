// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CredentialTarget, CredentialsSnapshot, IdentitySnapshot } from "../lib/types";
import { CredentialsPage } from "./CredentialsPage";

const api = vi.hoisted(() => ({ list: vi.fn(), forget: vi.fn(), identities: vi.fn(), save_identity: vi.fn(), forget_identity: vi.fn(), import_metadata: vi.fn() }));
vi.mock("../lib/tauri", () => ({ listSavedCredentials: api.list, forgetSavedCredential: api.forget, listIdentityFiles: api.identities, saveIdentityPassphrase: api.save_identity, forgetIdentityPassphrase: api.forget_identity, importCredentialMetadata: api.import_metadata }));
const credentials: CredentialsSnapshot = {
  metadata_import_required: true,
  credentials: [{ credential_id: "saved-item", name: "Development", kind: "ssh_password", storage: "keychain", target: "dev.example.test", account: "developer", created_at_ms: null, updated_at_ms: null, detail: null, action: "forget", vpn_connection_id: null }],
  sources: [{ source: "keychain", state: "ready", message: null }], checked_at_ms: 3000,
};
const identities: IdentitySnapshot = {
  metadata_import_required: true, keychain_message: null,
  identity_files: [{ identity_id: "identity", path: "/fixture/key", display_path: "/fixture/key", file_version: "version", key_type: null, fingerprint: null, encrypted: true, file_state: "ready", passphrase_state: "unknown", detail: null, used_by: [] }],
  complete: false, warning: null, keychain_available: true, checked_at_ms: 3000,
};
const targets: CredentialTarget[] = [];
const props = () => ({ visible: true, targets, on_close: vi.fn(), on_dialog_change: vi.fn(), on_manage_vpn: vi.fn() });
const importButton = () => screen.getByRole("button", { name: "Import saved credential metadata" });

beforeEach(() => {
  vi.resetAllMocks();
  api.list.mockResolvedValue(structuredClone(credentials));
  api.identities.mockResolvedValue(structuredClone(identities));
  api.import_metadata.mockResolvedValue(undefined);
});
afterEach(cleanup);

function importedSnapshots() {
  api.list.mockResolvedValue({ ...credentials, metadata_import_required: false });
  api.identities.mockResolvedValue({ ...identities, metadata_import_required: false, complete: true });
}

describe("Saved credential metadata import", () => {
  it.each(["credentials", "identities"])("offers explicit import when %s requires it without importing on open or refresh", async (source) => {
    api.list.mockResolvedValue({ ...credentials, metadata_import_required: source === "credentials" });
    api.identities.mockResolvedValue({ ...identities, metadata_import_required: source === "identities" });
    render(<CredentialsPage {...props()} />);
    await screen.findByRole("button", { name: "Import saved credential metadata" });
    const region = screen.getByRole("region", { name: "Saved credential metadata import" });
    expect(within(region).getByText(/Touch ID to read names and metadata from older protected SSH entries in Keychain/)).toBeTruthy();
    expect(importButton().getAttribute("aria-describedby")).toBe("credential-import-description");
    expect(api.import_metadata).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(api.identities).toHaveBeenCalledTimes(2));
    expect(api.import_metadata).not.toHaveBeenCalled();
  });

  it("defaults missing older snapshot flags to import needed and never claims absent passphrases", async () => {
    const { metadata_import_required: _credential_flag, ...older_credentials } = credentials;
    const { metadata_import_required: _identity_flag, keychain_message: _message, ...older_identities } = identities;
    api.list.mockResolvedValue({ ...older_credentials, credentials: [] });
    api.identities.mockResolvedValue({ ...older_identities, identity_files: [{ ...identities.identity_files[0], passphrase_state: "not_saved" }] });
    render(<CredentialsPage {...props()} />);
    await screen.findByRole("button", { name: "Import saved credential metadata" });
    expect(screen.getByText("Not checked")).toBeTruthy();
    expect(screen.queryByText("Not saved")).toBeNull();
    expect(screen.queryByText("No saved credentials found.")).toBeNull();
    expect(screen.getByText("Import saved metadata to check for additional credentials.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save passphrase for key" })).toHaveProperty("disabled", false);
    expect(screen.queryByRole("button", { name: "Forget passphrase for key" })).toBeNull();
    expect(screen.queryByText("Some identity files could not be checked.")).toBeNull();
  });

  it("shows the sanitized Keychain reason instead of a vague unavailable message", async () => {
    api.identities.mockResolvedValue({ ...identities, metadata_import_required: false, keychain_available: false, keychain_message: "This ctld helper is missing the Keychain access entitlement. Use a properly signed helper." });
    render(<CredentialsPage {...props()} />);
    expect(await screen.findByText("This ctld helper is missing the Keychain access entitlement. Use a properly signed helper.")).toBeTruthy();
    expect(screen.queryByText(/Keychain passphrase storage could not be checked/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Save passphrase for key" })).toBeNull();
    expect(screen.queryByText("Some identity files could not be checked.")).toBeNull();
  });

  it("keeps file discovery warnings separate from metadata import and Keychain status", async () => {
    api.identities.mockResolvedValueOnce({ ...identities, warning: "A configured identity file could not be read." })
      .mockResolvedValueOnce({ ...identities, metadata_import_required: false });
    render(<CredentialsPage {...props()} />);
    expect(await screen.findByText("A configured identity file could not be read.")).toBeTruthy();
    expect(screen.queryByText("Some identity files could not be checked.")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByText("Some identity files could not be checked.")).toBeTruthy();
  });

  it("offers an explicit reimport after completion to discover credentials saved by an older daemon", async () => {
    importedSnapshots();
    let complete!: () => void;
    api.import_metadata.mockImplementationOnce(() => new Promise<void>((resolve) => { complete = resolve; }));
    render(<CredentialsPage {...props()} />);
    const reimport = await screen.findByRole("button", { name: "Reimport saved credential metadata" });
    await waitFor(() => expect(reimport).toHaveProperty("disabled", false));
    expect(reimport.getAttribute("aria-describedby")).toBe("credential-reimport-description");
    expect(screen.getByText(/Import again if credentials were saved using an older ctld/)).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Saved credential metadata import" })).toBeNull();
    expect(api.import_metadata).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(reimport).toHaveProperty("disabled", false));
    expect(api.import_metadata).not.toHaveBeenCalled();

    fireEvent.click(reimport);
    fireEvent.click(reimport);
    expect(api.import_metadata).toHaveBeenCalledExactlyOnceWith();
    expect(screen.getByRole("button", { name: "Importing metadata…" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "Forget Development" })).toHaveProperty("disabled", true);
    api.list.mockResolvedValue({ ...credentials, metadata_import_required: false, credentials: [...credentials.credentials, { ...credentials.credentials[0], credential_id: "older-writer", name: "Older daemon entry" }] });
    await act(async () => complete());
    expect(await screen.findByText("Older daemon entry")).toBeTruthy();
    expect(screen.getByText("Saved credential metadata imported.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Reimport saved credential metadata" })).toHaveProperty("disabled", false);
    expect(api.list).toHaveBeenCalledTimes(3);
    expect(api.identities).toHaveBeenCalledTimes(3);
  });

  it("locks shared mutations, identifies the requested access, and confirms import only after both inventories refresh", async () => {
    let complete!: () => void;
    api.import_metadata.mockImplementationOnce(() => new Promise<void>((resolve) => { complete = resolve; }));
    const options = props();
    render(<CredentialsPage {...options} />);
    await screen.findByRole("button", { name: "Import saved credential metadata" });
    fireEvent.click(importButton());
    expect(api.import_metadata).toHaveBeenCalledExactlyOnceWith();
    expect(screen.getByRole("dialog", { name: "Import saved credential metadata" })).toBeTruthy();
    expect(screen.getByText("Accessing names and metadata for older saved SSH passwords and key passphrases in Keychain.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel quick input" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "Forget Development" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "Save passphrase for key" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "Refresh" })).toHaveProperty("disabled", true);
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(true);
    importedSnapshots();
    await act(async () => complete());
    expect(await screen.findByText("Saved credential metadata imported.")).toBeTruthy();
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(api.identities).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("button", { name: "Import saved credential metadata" })).toBeNull();
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(false);
  });

  it("keeps import available when a partial result still requires it", async () => {
    render(<CredentialsPage {...props()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Import saved credential metadata" }));
    expect(await screen.findByText("Imported available metadata. Some saved entries still need metadata import.")).toBeTruthy();
    expect(importButton()).toHaveProperty("disabled", false);
    expect(screen.queryByText("Saved credential metadata imported.")).toBeNull();
  });

  it.each([
    ["credential_store_busy", "Another Keychain request is still active. Complete or cancel it, then try importing again."],
    ["identity_keychain_busy", "Another Keychain request is still active. Complete or cancel it, then try importing again."],
    ["credential_store_locked", "Metadata import was not completed because Keychain access was locked, denied, or cancelled. Try again when ready to allow access."],
    ["credential_store_missing_entitlement", "The credential helper is not authorized for Keychain access. Use the signed ctld app with its matching provisioning profile, then try the import again."],
    ["identity_keychain_missing_entitlement", "The credential helper is not authorized for Keychain access. Use the signed ctld app with its matching provisioning profile, then try the import again."],
    ["credential_store_unavailable", "Keychain access is unavailable. Check your macOS login session, then try the import again."],
    ["credential_import_failed", "Saved credential metadata could not be fully imported. Existing credentials are unchanged; try importing again."],
    ["constructor", "Could not import saved credential metadata. Check Keychain access and update ctld if needed, then try again."],
  ])("refreshes available metadata after %s without hiding required state", async (code, message) => {
    api.import_metadata.mockRejectedValue({ code, message: "private-error-canary" });
    render(<CredentialsPage {...props()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Import saved credential metadata" }));
    expect(await screen.findByText(message)).toBeTruthy();
    await waitFor(() => expect(api.identities).toHaveBeenCalledTimes(2));
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(importButton()).toHaveProperty("disabled", false);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.body.textContent).not.toContain("private-error-canary");
    expect(screen.queryByText("Saved credential metadata imported.")).toBeNull();
  });

  it("does not claim full completion when the post-import inventory cannot be refreshed", async () => {
    render(<CredentialsPage {...props()} />);
    await screen.findByRole("button", { name: "Import saved credential metadata" });
    api.list.mockRejectedValue(new Error("Metadata unavailable."));
    api.identities.mockResolvedValue({ ...identities, metadata_import_required: false });
    fireEvent.click(importButton());
    expect(await screen.findByText("Metadata import finished. Refresh to check whether all saved entries were imported.")).toBeTruthy();
    expect(importButton()).toHaveProperty("disabled", false);
    expect(screen.queryByText("Saved credential metadata imported.")).toBeNull();
  });

  it("does not restore a pending-import state from a refresh started before import", async () => {
    let complete!: (value: IdentitySnapshot) => void;
    api.identities.mockResolvedValueOnce(identities)
      .mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }))
      .mockResolvedValueOnce(identities);
    const options = props();
    const page = render(<CredentialsPage {...options} />);
    await screen.findByRole("button", { name: "Import saved credential metadata" });
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    page.rerender(<CredentialsPage {...options} visible={false} />);
    page.rerender(<CredentialsPage {...options} />);
    await waitFor(() => expect(importButton()).toHaveProperty("disabled", false));
    importedSnapshots();
    fireEvent.click(importButton());
    await screen.findByText("Saved credential metadata imported.");
    await act(async () => complete(identities));
    expect(screen.queryByRole("button", { name: "Import saved credential metadata" })).toBeNull();
    expect(api.import_metadata).toHaveBeenCalledOnce();
  });

  it("allows leaving the page while import continues and refreshes on return", async () => {
    let complete!: () => void;
    api.import_metadata.mockImplementationOnce(() => new Promise<void>((resolve) => { complete = resolve; }));
    const options = props();
    const page = render(<CredentialsPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Import saved credential metadata" }));
    page.rerender(<CredentialsPage {...options} visible={false} />);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(false);
    importedSnapshots();
    await act(async () => complete());
    page.rerender(<CredentialsPage {...options} />);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Import saved credential metadata" })).toBeNull());
    expect(api.identities).toHaveBeenCalledTimes(2);
    expect(api.import_metadata).toHaveBeenCalledOnce();
  });
});
