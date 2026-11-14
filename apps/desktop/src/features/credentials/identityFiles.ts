import { message, translate, type Locale } from "../i18n/messages";
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

export function identityPassphraseMessage(file: IdentityFile) {
  return {
    saved: message("credentials.passphrase_state.saved"),
    not_saved: message("credentials.passphrase_state.not_saved"),
    not_required: message("credentials.passphrase_state.not_required"),
    file_changed: message("credentials.passphrase_state.file_changed"),
    unknown: message("credentials.passphrase_state.unknown"),
  }[file.passphrase_state];
}

export function identityMutationError(failure: unknown, action: "save" | "forget" = "save", locale: Locale = "en"): string {
  return translate(locale, identityMutationMessage(failure, action).id);
}

function identityErrorCode(failure: unknown): string {
  const candidate = typeof failure === "object" && failure !== null && "code" in failure ? failure.code : failure;
  return typeof candidate === "string" ? candidate : "";
}

export function isIdentityPassphraseError(failure: unknown): boolean {
  return identityErrorCode(failure) === "identity_unlock_failed";
}

/** Only known codes are rendered; backend error text may contain the submitted secret. */
export function identityMutationMessage(failure: unknown, action: "save" | "forget" = "save") {
  const code = identityErrorCode(failure);
  const messages: Record<string, string> = {
    identity_invalid_request: "credentials.mutation.identity_invalid_request",
    identity_file_changed: "credentials.mutation.identity_file_changed",
    identity_file_missing: "credentials.mutation.identity_file_missing",
    identity_file_unreadable: "credentials.mutation.identity_file_unreadable",
    identity_unlock_failed: "credentials.mutation.identity_unlock_failed",
    credential_store_busy: "credentials.mutation.keychain_busy",
    identity_keychain_busy: "credentials.mutation.keychain_busy",
    identity_keychain_missing_entitlement: "credentials.mutation.identity_keychain_missing_entitlement",
    identity_keychain_unavailable: "credentials.mutation.identity_keychain_unavailable",
    identity_keychain_locked: "credentials.mutation.identity_keychain_locked",
    identity_unsupported: "credentials.mutation.identity_unsupported",
    identity_save_failed: "credentials.mutation.identity_save_failed",
    identity_forget_failed: "credentials.mutation.identity_forget_failed",
    identity_list_failed: "credentials.mutation.identity_list_failed",
    credential_helper_unavailable: "credentials.mutation.credential_helper_unavailable",
    credential_helper_unsupported: "credentials.mutation.credential_helper_unsupported",
    credential_helper_invalid_response: "credentials.mutation.credential_helper_invalid_response",
    credential_helper_timeout: "credentials.mutation.credential_helper_timeout",
  };
  if (Object.prototype.hasOwnProperty.call(messages, code)) return message(messages[code]);
  return message(action === "forget" ? "credentials.mutation.identity_forget_failed" : "credentials.mutation.save_unknown");
}
