import { message } from "../i18n/messages";

/** Older snapshots cannot prove that protected metadata has been imported. */
export function metadataImportRequired(snapshot: { metadata_import_required?: boolean } | null): boolean {
  return snapshot !== null && snapshot.metadata_import_required !== false;
}

/** Only known codes are rendered; native diagnostics never become UI copy. */
export function credentialImportError(failure: unknown) {
  const candidate = typeof failure === "object" && failure !== null && "code" in failure ? failure.code : failure;
  const code = typeof candidate === "string" ? candidate : "";
  const messages: Record<string, string> = {
    credential_import_failed: "credentials.metadata_import.failed",
    credential_store_busy: "credentials.metadata_import.busy",
    identity_keychain_busy: "credentials.metadata_import.busy",
    credential_store_locked: "credentials.metadata_import.locked",
    identity_keychain_locked: "credentials.metadata_import.locked",
    credential_store_missing_entitlement: "credentials.metadata_import.missing_entitlement",
    identity_keychain_missing_entitlement: "credentials.metadata_import.missing_entitlement",
    credential_store_unavailable: "credentials.metadata_import.unavailable",
    identity_keychain_unavailable: "credentials.metadata_import.unavailable",
    credentials_unsupported: "credentials.metadata_import.unsupported",
    credential_store_unsupported: "credentials.metadata_import.unsupported",
    credential_helper_unavailable: "credentials.metadata_import.helper_unavailable",
    credential_helper_unsupported: "credentials.metadata_import.helper_unsupported",
    credential_helper_invalid_response: "credentials.metadata_import.helper_invalid_response",
    credential_helper_timeout: "credentials.metadata_import.helper_timeout",
  };
  if (Object.prototype.hasOwnProperty.call(messages, code)) return message(messages[code]);
  return message("credentials.metadata_import.unknown");
}
