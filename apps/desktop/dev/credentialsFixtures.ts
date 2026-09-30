import type { CredentialsSnapshot, IdentitySnapshot } from "../src/lib/types";

export function previewCredentials(): CredentialsSnapshot {
  return {
    credentials: [
      { credential_id: "preview-ssh-password", name: "Development", kind: "ssh_password", storage: "keychain", target: "dev.example.test", account: "developer", created_at_ms: Date.UTC(2026, 8, 20), updated_at_ms: Date.UTC(2026, 8, 28), detail: "Requires Touch ID when used.", action: "forget", vpn_connection_id: null },
      { credential_id: "preview-passphrase", name: "Development key", kind: "ssh_key_passphrase", storage: "keychain", target: "dev.example.test", account: "~/.ssh/id_ed25519", created_at_ms: Date.UTC(2026, 8, 21), updated_at_ms: Date.UTC(2026, 8, 21), detail: "Requires Touch ID when used.", action: "forget", vpn_connection_id: null },
      { credential_id: "preview-legacy", name: "Saved SSH credential", kind: "ssh_credential", storage: "keychain", target: null, account: null, created_at_ms: null, updated_at_ms: null, detail: "This older item does not include a readable name or credential type.", action: "forget", vpn_connection_id: null },
      { credential_id: "preview-vpn", name: "Work VPN", kind: "vpn_password", storage: "vpn_settings", target: "vpn.example.test", account: "sample", created_at_ms: null, updated_at_ms: null, detail: "Saved in private VPN settings.", action: "manage_vpn", vpn_connection_id: "work-vpn" },
      { credential_id: "preview-tailscale", name: "Team network", kind: "tailscale_sign_in", storage: "container_volume", target: null, account: null, created_at_ms: null, updated_at_ms: null, detail: "Saved Tailscale profile. Sign-in data in its container volume has not been verified.", action: "manage_vpn", vpn_connection_id: "tailnet-vpn" },
    ],
    sources: [{ source: "keychain", state: "ready", message: null }, { source: "vpn", state: "ready", message: null }],
    checked_at_ms: Date.UTC(2026, 8, 29, 12),
  };
}

export function previewIdentityFiles(): IdentitySnapshot {
  return {
    identity_files: [
      { identity_id: "preview-key-development", path: "/Users/sample/.ssh/id_ed25519", display_path: "~/.ssh/id_ed25519", file_version: "preview-version-1", key_type: "Ed25519", fingerprint: "SHA256:syntheticDevelopmentKeyFingerprint", encrypted: true, file_state: "ready", passphrase_state: "saved", detail: "Saved in Keychain for this version of the file.", used_by: ["Development", "Staging"] },
      { identity_id: "preview-key-new", path: "/Users/sample/.ssh/operations", display_path: "~/.ssh/operations", file_version: "preview-version-2", key_type: "RSA", fingerprint: "SHA256:syntheticOperationsKeyFingerprint", encrypted: true, file_state: "ready", passphrase_state: "not_saved", detail: null, used_by: [] },
      { identity_id: "preview-key-changed", path: "/Users/sample/.ssh/backup", display_path: "~/.ssh/backup", file_version: "preview-version-3", key_type: "Ed25519", fingerprint: "SHA256:syntheticBackupKeyFingerprint", encrypted: true, file_state: "ready", passphrase_state: "file_changed", detail: "The saved passphrase belongs to an older version of this file.", used_by: ["Backup"] },
      { identity_id: "preview-key-open", path: "/Users/sample/.ssh/local_test", display_path: "~/.ssh/local_test", file_version: "preview-version-4", key_type: "Ed25519", fingerprint: "SHA256:syntheticLocalKeyFingerprint", encrypted: false, file_state: "ready", passphrase_state: "not_required", detail: "This key is not encrypted.", used_by: [] },
    ],
    complete: true,
    warning: null,
    keychain_available: true,
    checked_at_ms: Date.UTC(2026, 8, 30, 12),
  };
}
