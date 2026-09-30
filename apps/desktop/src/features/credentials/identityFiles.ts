import type { IdentityFile } from "../../lib/types";

export function identityName(file: IdentityFile): string {
  return file.display_path.split(/[\\/]/).pop() || file.display_path;
}

export function canSaveIdentity(file: IdentityFile, keychain_available: boolean): boolean {
  return keychain_available && file.file_state === "ready" && file.encrypted === true && Boolean(file.file_version);
}

export function canForgetIdentity(file: IdentityFile, keychain_available: boolean): boolean {
  return keychain_available && (file.passphrase_state === "saved" || file.passphrase_state === "file_changed");
}

export function identityPassphraseLabel(file: IdentityFile): string {
  return {
    saved: "Saved",
    not_saved: "Not saved",
    not_required: "Not required",
    file_changed: "File changed · saved for older file",
    unknown: "Not checked",
  }[file.passphrase_state];
}

/** Only known codes are rendered; backend error text may contain the submitted secret. */
export function identityMutationError(failure: unknown, action: "save" | "forget" = "save"): string {
  const candidate = typeof failure === "object" && failure !== null && "code" in failure ? failure.code : failure;
  const code = typeof candidate === "string" ? candidate : "";
  const messages: Record<string, string> = {
    identity_invalid_request: "The identity file request is invalid. Refresh and try again.",
    identity_file_changed: "The identity file changed. Refresh and try again with the current file.",
    identity_file_missing: "The identity file is missing. Check its path, then refresh.",
    identity_file_unreadable: "The identity file could not be read. Check its permissions, then refresh.",
    identity_unlock_failed: "The passphrase does not unlock this identity file. Try again.",
    credential_store_busy: "Another Keychain request is still active. Complete or cancel it, then try again.",
    identity_keychain_busy: "Another Keychain request is still active. Complete or cancel it, then try again.",
    identity_keychain_unavailable: "Keychain is unavailable. Check access and try again.",
    identity_keychain_locked: "Keychain access was denied or locked. Unlock it and try again.",
    identity_unsupported: "This identity file or platform does not support saved passphrases.",
    identity_save_failed: "The verified passphrase could not be saved to Keychain. Check access and try again.",
    identity_forget_failed: "The saved passphrase could not be forgotten. Check Keychain access and try again.",
    identity_list_failed: "Identity file metadata could not be checked. Refresh and try again.",
    credential_helper_unavailable: "The credential helper is unavailable. Rebuild or update ctld and try again.",
    credential_helper_unsupported: "This version of ctld does not support identity passphrases. Update ctld and try again.",
    credential_helper_invalid_response: "The credential helper returned an invalid response. Update ctld and try again.",
    credential_helper_timeout: "The credential operation timed out. Refresh to check the current state before trying again.",
  };
  if (Object.prototype.hasOwnProperty.call(messages, code)) return messages[code];
  return action === "forget"
    ? "The saved passphrase could not be forgotten. Check Keychain access and try again."
    : "The passphrase could not be saved. Check the passphrase, identity file, and Keychain access, then try again.";
}
