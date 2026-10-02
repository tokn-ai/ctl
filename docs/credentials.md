# Saved credentials

Open **Credentials** from the sidebar or command palette to inspect credentials
managed by ctmux and local SSH identity files. The page displays names, credential
types, accounts or targets, storage locations, and recorded dates. It has no reveal or copy-secret action.

## Identity files

Each identity file has one row with its name and path, verified key type and
fingerprint when available, recorded host references, and passphrase status.
The inventory includes paths configured by saved hosts and their gateways (including
unavailable hosts), literal `IdentityFile` paths from SSH config and included
files, keys discovered directly in `~/.ssh`, and paths attached to saved ctmux
identity passphrases. Symlinks to the same existing file share a row. Discovery
does not connect to hosts or execute SSH config `Match exec` commands. Paths
requiring connection-specific expansion and bounded discovery omissions produce
an incomplete-inventory warning. **Used by** lists known saved-host references;
**Not recorded** does not mean a file is unused.

- **Save passphrase** verifies that the entered passphrase unlocks the current
  file locally, then stores it in macOS Keychain. The private key stays in its
  file. **Replace** performs the same verification before updating a saved item.
- **Saved** means a Keychain item matches this path and file version. It does not
  indicate an active SSH connection. **Not saved** means no matching saved item
  was found; **Not required** means the file is unencrypted.
- **File changed · saved for older file** prevents reuse after replacement or
  re-encryption. Verify and replace the saved passphrase, or forget the old item.
  Missing or unreadable files remain visible when configured or saved.
- **Not checked** means saved status could not be determined. Inaccessible
  Keychain storage does not become a false **Not saved** result.
- **Forget** removes only the selected saved passphrase after confirmation. It
  never deletes or edits the identity file and does not terminate connections.

Passphrase fields are masked and cleared on submission, cancellation, leaving
Credentials, and unmount. A submitted native operation continues if the page is
closed; leaving the page does not claim to cancel a Keychain write. Refresh
checks both identity metadata and the other saved credentials independently.
Unsupported key formats and unavailable Keychain access are shown explicitly.
Key type and fingerprint remain unverified until local unlock has established
them for that exact file version; a neighbouring `.pub` file is not proof.

When connecting with an identity file, a newly entered key passphrase is saved
only after local verification, even if SSH successfully authenticated by another
method. Previously saved identity passphrases can be reused across hosts that
use the same file. Older prompt-scoped key passphrases remain visible as legacy
credential rows; they are not silently treated as verified identity entries.

## Storage and metadata

- SSH passwords and SSH key passphrases are stored in the macOS protected
  Keychain. Display metadata lives in separate, non-biometric Keychain records
  in the same app access group. Inventory queries explicitly forbid authentication
  UI and never request password data. An inaccessible Keychain is reported as
  unavailable rather than empty.
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

**Import saved credential metadata** is an explicit action for entries saved
before this metadata index existed. It requests Touch ID to read their names and
attributes, then writes metadata records without changing the protected secrets.
An interrupted or incomplete import remains available to retry. Until import
completes, missing metadata means **Not checked**, not **Not saved**. Opening the
page and pressing Refresh never start this import or display authentication UI.
**Reimport saved credential metadata** remains available afterward, for example
if an older ctld saved additional credentials without updating this index. Update
or restart that daemon to use named prompts and keep new metadata synchronized;
the page does not restart active SSH connections automatically.

Authentication requests describe their purpose: the key file whose passphrase
is being read or replaced, the SSH account and destination whose password is
needed, or the explicit import of previously saved ctmux credential metadata.
Connection setup discovers public identities without unlocking saved keys. Only
an SSH signature request for a configured key can read its saved passphrase and
protected binding together. Password-only connections therefore do not request
identity-passphrase access; a gateway or server that actually requires both
credentials can still need separate authorizations.

When OpenSSH needs manual password or key-passphrase entry, the authentication
prompt explains whether no usable saved credential was found or Keychain access
was unavailable, unauthorized, locked, or denied. This warning does not appear
for unused keys or an already-authenticated master. Before offering to save a
credential, ctld checks its current Keychain access without reading a password
or displaying authentication UI. If access is unavailable, it completes the
connection without a save offer or an extra passphrase import.

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
with `--credential-request` for existing SSH credential metadata and exact-item
deletion, or `--identity-request` for identity listing, verified saving, and
exact identity-passphrase deletion. Both use bounded JSON over private process
pipes. Passphrases are never command-line arguments, environment variables,
logs, response fields, or frontend inventory data. Secret-bearing native
buffers are zeroized; helper diagnostics are converted to fixed error codes
and messages. This works independently of the running daemon,
without changing the SSH/VPN wire protocol or restarting active connections.
The noninteractive list request has its own operation name, so an older helper
rejects it before performing an authenticated legacy inventory.
An older helper must be updated or rebuilt to support this page.

Keychain results are restricted to ctmux's credential namespace; saved "never
save" preferences and credentials owned by other applications are excluded.
Malformed requests, inaccessible storage, and oversized inventories are
reported explicitly. Tests use synthetic metadata and helper processes, without
reading or deleting the user's Keychain entries.

Identity passphrases use a separate Keychain namespace. The account identifies
the canonical key path; nonsecret metadata binds the saved item to a digest of
the file bytes and a locally verified public fingerprint. Replacement or
re-encryption invalidates reuse, even if the public key is unchanged. Inventory
reads the separate metadata records without authentication. Before using a
passphrase, the connection reads its protected binding and secret together and
validates the binding against the current file; cached display metadata never
authorizes secret reuse.

Local verification uses an isolated temporary OpenSSH agent and a private
askpass channel. The key snapshot is provided through stdin, and the saved
passphrase is never returned to an SSH password prompt. Preparation advertises
public identities for saved keys while preserving the configured agent and
native fallback for unsaved keys. It reads no passphrase. When SSH requests a
signature, the requested key is unlocked in a temporary local agent and its
verified public identity must match the advertised key before signing. Repeated
requests share that unlock within the connection attempt; cancellation disposes
of the temporary agent and prevents queued credential reads from starting.
An authentication dialog already displayed by macOS remains under OS control.
An existing authenticated SSH master is reused before this preparation, and
keys already available from the original agent do not need another Keychain read.

OpenSSH-format files provide their public identity without decryption. Opaque
PEM files can use a companion `.pub` file or public metadata recorded when their
passphrase was saved. These are only discovery hints, never authority to use a
secret or sign with a different key. Older PEM files without either source keep
native passphrase entry; saving the verified passphrase again records the public
metadata for future connections. Explicit `IdentityAgent=none` and authentication
preferences that exclude public keys retain native behavior. Configured SSH
gateways and bounded `ProxyJump` chains participate when they inherit that agent;
gateway-specific `IdentityAgent` settings retain native authentication. Gateway
PEM keys without a readable public identity may still
prompt when `IdentitiesOnly=yes`; no gateway command or SSH configuration is
rewritten. Configurations that enable `AddKeysToAgent` also keep native
authentication so their existing agent behavior is preserved. These cases can
still save passphrases after local verification. Agent forwarding remains
disabled for ctld-created masters, as before.
The running ctld must include this support for reuse during new connections;
the Credentials page's one-shot helper does not restart it automatically.
