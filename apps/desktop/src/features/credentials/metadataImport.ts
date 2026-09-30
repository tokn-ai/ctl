/** Older snapshots cannot prove that protected metadata has been imported. */
export function metadataImportRequired(snapshot: { metadata_import_required?: boolean } | null): boolean {
  return snapshot !== null && snapshot.metadata_import_required !== false;
}

/** Only known codes are rendered; native diagnostics never become UI copy. */
export function credentialImportError(failure: unknown): string {
  const candidate = typeof failure === "object" && failure !== null && "code" in failure ? failure.code : failure;
  const code = typeof candidate === "string" ? candidate : "";
  const messages: Record<string, string> = {
    credential_import_failed: "Saved credential metadata could not be fully imported. Existing credentials are unchanged; try importing again.",
    credential_store_busy: "Another Keychain request is still active. Complete or cancel it, then try importing again.",
    identity_keychain_busy: "Another Keychain request is still active. Complete or cancel it, then try importing again.",
    credential_store_locked: "Metadata import was not completed because Keychain access was locked, denied, or cancelled. Try again when ready to allow access.",
    identity_keychain_locked: "Metadata import was not completed because Keychain access was locked, denied, or cancelled. Try again when ready to allow access.",
    credential_store_unavailable: "The credential helper cannot access Keychain. Use a properly signed ctld helper, then try the import again.",
    identity_keychain_unavailable: "The credential helper cannot access Keychain. Use a properly signed ctld helper, then try the import again.",
    credentials_unsupported: "Importing saved credential metadata requires macOS Keychain.",
    credential_store_unsupported: "Importing saved credential metadata requires macOS Keychain.",
    credential_helper_unavailable: "The credential helper is unavailable. Update or rebuild ctld, then try the import again.",
    credential_helper_unsupported: "This version of ctld does not support metadata import. Update ctld and try again.",
    credential_helper_invalid_response: "The credential helper returned an invalid response. Update ctld and try the import again.",
    credential_helper_timeout: "Metadata import timed out. Available metadata has been refreshed; remaining entries may still need import.",
  };
  if (Object.prototype.hasOwnProperty.call(messages, code)) return messages[code];
  return "Could not import saved credential metadata. Check Keychain access and update ctld if needed, then try again.";
}
