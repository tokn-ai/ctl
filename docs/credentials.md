# Saved credentials

Open **Credentials** from the sidebar or command palette to inspect credentials
managed by rmux. The page displays names, credential types, accounts or targets,
storage locations, and recorded dates. It has no reveal or copy-secret action.

## Storage and metadata

- SSH passwords and SSH key passphrases are stored in the macOS protected
  Keychain. Inventory requests ask for item attributes only, never password
  data. An inaccessible Keychain is reported as unavailable rather than empty.
- Older SSH entries contain hashed identifiers without readable metadata. The
  app associates them with currently configured host routes when possible;
  otherwise they remain **Saved SSH credential** entries with a short identifier.
  Their password/passphrase subtype is not guessed. New saves include nonsecret
  metadata while retaining the existing credential identity and access policy.
- OpenConnect password rows come from saved VPN profiles. Their storage location
  is the private VPN settings file, not Keychain. The native response excludes
  the password and strips user information, paths, and query parameters from the
  displayed gateway URL.
- Tailscale rows identify saved connections whose sign-in is managed in a
  container volume. A saved connection alone does not prove that sign-in state
  still exists; these rows explicitly leave that state unverified.

Creation and modification dates are Keychain metadata, not a record of the last
login. Missing dates are shown as not recorded. Source errors do not hide rows
successfully read from another source, and a failed refresh marks retained rows
as the previous result.

## Forgetting credentials

**Forget** removes the single selected SSH Keychain item after confirmation.
It does not disconnect SSH sessions or change the host's save-credential policy.
The credential may be requested again when a new connection needs it. This is
separate from the existing host-wide credential cleanup.

VPN rows use **Manage VPN** to open existing VPN settings and sign-in controls.
They do not remove a saved connection as a side effect of credential inspection.

## Native boundary

The app invokes the selected signed `ctld` executable as a short-lived helper
with `--credential-request`. Its bounded JSON request supports metadata listing
and exact-item deletion only. This works independently of the running daemon,
without changing the SSH/VPN wire protocol or restarting active connections.
An older helper must be updated or rebuilt to support this page.

Keychain results are restricted to rmux's credential namespace; saved "never
save" preferences and credentials owned by other applications are excluded.
Malformed requests, inaccessible storage, and oversized inventories are
reported explicitly. Tests use synthetic metadata and helper processes, without
reading or deleting the user's Keychain entries.
