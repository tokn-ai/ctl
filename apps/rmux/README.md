# rmux app

The desktop client for local and SSH-connected daemon-owned `rmux` terminal
sessions. It uses Tauri 2, React/TypeScript, and xterm.js.

## Develop

From the repository root, install the frontend dependencies and start Tauri:

```sh
cd apps/rmux
pnpm install
pnpm tauri dev
```

Tauri development startup builds `ctld`, `rmuxd`, and `taskd` beside the app's
Cargo binary so fresh starts use matching local daemon protocols. `pnpm dev`
still starts only the frontend; `pnpm daemons:build` rebuilds the local daemons
separately. After changing a daemon protocol during development, restart the
affected daemon once its connections are idle; rebuilding does not replace an
already running process.

Development startup also performs a local-only bundle preflight. It warns but does
not block local or already-provisioned SSH work when remote install bundles are
absent. To test installation on a new SSH host, first commit and push the
current branch, then run:

```sh
pnpm agents:sync
```

The command reuses or dispatches the bundle workflow for the exact commit,
waits for it, verifies all four archives, and stages them in the ignored Tauri
resource directory. `pnpm agents:sync --main` is an explicit compatibility
shortcut for using the latest successful main-branch set.

The app may also use the path in `RMUXD_BIN`. A saved host represents a named
machine with one remote account/environment. Addresses and gateway routes are
named connection methods on that host. **Add host** guides you through
`[user@]hostname[:port]` (or an SSH config alias), a display name, and
authentication. After verification, it automatically saves the named host and
its first `SSH` method in `~/.tokn/rmux/hosts.json`. Display names may contain
spaces; they are independent of SSH aliases. No storage-choice step or implicit
OpenSSH config write is involved.

Concrete aliases discovered from `~/.ssh/config`, including its `Include`
files, appear as hosts in memory. OpenSSH continues resolving their connection
settings. Connecting does not save their definitions; saving a customization
from **Host settings** creates a saved host with the same ID. A saved pure alias
method suppresses an otherwise-unused duplicate projection. Missing aliases
with workspace references remain visible as unavailable; restoring the alias
restores access without losing session or task references.

The connection-method editor supports direct SSH, optional identity-file paths,
and ordered routes through reusable gateways. **Verify and save** verifies the
remote environment and saves the method in the host catalog. For a new direct
method, **Also save to OpenSSH config** optionally exports a managed `Host`
block after verification. This option is off by default and unavailable for
existing config aliases or gateway routes; the saved method remains in the
host catalog. OpenSSH requests host-key confirmation, passwords,
passphrases, or interactive responses through the quick-input overlay. If
`ctl-agent` is missing, packaged builds can install the matching
checksummed `ctl-agent`, `rmuxd`, and `taskd` bundle for the remote user. Custom
command paths are not supported. Each development commit has a distinct bundle
ID, so a remote host cannot silently retain an older build with the same app
version. App-local connection settings are supplied to OpenSSH as fixed
arguments; methods based on an existing SSH config alias retain that alias.
On macOS, verified passwords and private-key passphrases are stored separately in the device-local,
Touch ID-protected Keychain. Private-key contents, arbitrary options,
forwarding, and remote commands are never stored. A per-user `ctld` process
owns the authenticated OpenSSH control masters and is the only local component
that accesses Keychain; the desktop client only forwards attempt-scoped
prompts. Masters remain available for five idle minutes so background channels
do not race a discarded authentication connection.

Open **Host settings** from the host row to rename the machine or its methods,
add or edit a connection, remove a method while retaining at least one, or
**Make preferred**. **Connect host** uses the preferred method. **Connect using**
chooses a specific method for the current connection without changing that
preference; there is no automatic fallback. Saving names or preferences does not
connect. Adding or editing a method verifies its endpoint before saving, but
does not switch an existing session's transport. Use an explicit connection to
apply that route to the host's remembered sessions.

All methods on a host must reach its verified account-owned ctl environment.
Use a separate host for another account. Matching remote IDs never merge saved
hosts automatically. Renaming a host or changing methods preserves session,
tab, task, and port-forward ownership through the stable `host_id`.
Existing WebView host settings migrate automatically after a successful disk write.

Saved host definitions and reusable gateways live in `~/.tokn/rmux/hosts.json`;
sessions, tabs, tasks, forwarding, and observed remote identities live in
`~/.tokn/rmux/workspace.json`. On first load, an existing workspace is imported
from Tauri's former app-data directory if the new file does not exist; the
original remains available for recovery. Schema 8 moves existing hosts and
gateways into the catalog before removing them from the workspace. Schema 7 is
backed up as `workspace-v7.backup.json`; earlier schemas retain their corresponding
backups. Host IDs and all session/task/port references remain unchanged.
It remembers known sessions, cached paths, tab order, and selection. Startup
restores those entries as unverified and automatically connects the selected
local tab. Remote terminal tabs stay disconnected until explicitly opened; **Connect
host** resumes that host's selected tab, or its first open tab if another host
was selected. No daemon inventory is discovered automatically. Use **Add
existing session** in the sidebar or command palette to discover one host's
inventory and explicitly remember sessions without attaching. **Refresh Known
Sessions** inspects only remembered IDs; it does not adopt other apps' sessions.
Old sessions were never saved, so the first migration requires explicit import.
See [workspace persistence](../../docs/rmux-workspace.md) for recovery and tests.

The Ports activity panel lists every saved local forward across SSH hosts,
including stopped entries, and shows `ctld`'s current active, waiting, or error
state. Start and Stop act on the shared `ctld` owner; selecting a row opens the
host-specific forwarding dialog for discovery and editing. Runtime state is
refreshed on launch, when the panel opens, and on demand, but is not persisted.

The identity-file input suggests candidate files from the top level of
`~/.ssh`. Type to filter, use the arrow keys and Enter, or click a file. Manual
paths remain available, including when discovery fails. Rust lists names and
metadata only: it does not open key contents. Public keys, common SSH support
files, backups, and directories are omitted; symlinks to regular files are
supported. Suggestions are not proof that a file is a valid private key—OpenSSH
validates the selected identity when connecting.

Wildcard and negated `Host` patterns are not destinations and are omitted from
suggestions. Discovery only fills the editor: the app contacts a candidate when
**Verify and save** or an explicit connection is requested. Local is always
present and remains the default for a new shell. The sidebar groups remembered
sessions under their named hosts. A failed host reports its own error while
last-known sessions from other targets remain usable.

SSH uses `ctl-core` and the system `ssh` executable with a fixed remote command
that prepends the app-managed directory before running `ctl-agent connect`;
forwarding, agent access, X11, local commands, and PTY allocation remain
disabled. On macOS/Linux, the per-user `ctld` owns one explicit OpenSSH control
master for each connection configuration. Its owner-only Unix socket carries
askpass requests to the quick-input UI for an active connection attempt. Host-key trust
requires explicit confirmation and is managed by OpenSSH. A master remains
available for five idle minutes, while all background channels require that
master and cannot independently prompt or fall back to another connection.

On macOS, only `ctld` links the Keychain implementation. Verified passwords and
key passphrases are stored device-locally under a Touch ID-only policy tied to
the currently enrolled fingerprints. They are loaded only inside `ctld` to
satisfy one OpenSSH prompt and never return to the desktop client. Native
plaintext buffers are zeroized after use; secrets never appear in command
arguments, environment variables, or logs, and one-time responses are not
stored. Removing a host asks `ctld` to delete credentials for its saved and active methods, retaining any credential scope still used by another host.
On Linux, `ctld` keeps a newly entered reusable secret only through authentication and
then discards it. An explicit interactive attempt allows up to three minutes
and Escape cancels it. On non-Unix platforms, preconfigured noninteractive SSH
remains available.

On macOS, `ctld` is packaged as the app-like helper
`rmux.app/Contents/Helpers/ctld.app`. Release builds sign that helper with the
permanent `io.rmux.desktop.ctld` bundle identifier and embed its matching
Developer ID provisioning profile. This gives `ctld` its own Keychain identity;
the main app and the other sidecars receive no credential-access entitlement.
Without the profile-authorized application identifier, the Data Protection
Keychain rejects credential storage and rmux reports the signing error instead
of silently weakening the access policy.

For local Touch ID testing with any Apple Account, first run
`pnpm tauri:dev:provision`. In the Xcode project it opens, select the
`ctld-provisioning` target, choose your Personal Team under **Signing &
Capabilities**, and build once. This Xcode project is copied under `target/`,
so the local team selection does not modify tracked files. Free Personal Team
profiles expire after seven days; after initial setup the signed-development
launcher asks Xcode to refresh an expired profile automatically.

Then run `pnpm tauri:dev:signed`. The launcher searches Xcode's downloaded
profiles and `~/Library/Application Support/rmux/signing/ctld.provisionprofile`,
selects the newest unexpired profile for `io.rmux.desktop.ctld`, discovers its
matching signing certificate in the login Keychain, and runs an isolated signed
`ctld` for the lifetime of `tauri dev`. It needs no signing environment
variables. Ordinary `pnpm tauri dev` remains unsigned and cannot store Touch
ID-protected credentials.

The release workflow derives the Team ID and signing identity from the profile
and imported certificate. It expects `APPLE_API_ISSUER` and `APPLE_API_KEY` as
non-secret repository variables. `APPLE_CERTIFICATE`,
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_CTLD_PROVISIONING_PROFILE`, and
`APPLE_API_KEY_CONTENT` are repository secrets. The certificate and profile
must be for Developer ID distribution, and the profile must authorize exactly
the team-prefixed `io.rmux.desktop.ctld` application identifier.

GUI-created shells receive an automatic `session-N` name. **Disconnect**
removes an open tab while leaving its shell running. For the active tab it also
detaches the live view; inactive tabs have no live attachment to detach.
**Terminate session** is deliberately destructive: after confirmation it terminates the
session for all clients. **Remove from workspace** forgets an entry and closes
its tab without terminating its shell. Closing the app itself only detaches its
active view. Normal window close waits for pending workspace saves; save failures
remain visible with a retry action.

The desktop normally has one native window and one WebView. A session and tab
are identified by both host and daemon session ID, so equal IDs from different
daemons remain distinct. Only the active tab holds an attachment.
Switching or closing tabs does not terminate their daemon-owned sessions.
The active tab and native window title show the last observed `path — command`
(or shell name while idle). Inactive tabs retain their last title snapshot in
this window until they are selected again, when they receive a fresh attachment.
If xterm is still starting, the latest selected tab remains in an attaching
state and is connected as soon as the renderer is ready. A failed attachment
leaves its tab selected and can be retried from the session list or command
palette.

Open the command palette with `Cmd-Shift-P` on macOS or `Ctrl-Shift-P` on
Windows/Linux. It exposes session creation, refresh, switching, disconnect and
close, plus terminal input, layout, reconnect, focus, and a destructive
`Restart rmuxd` maintenance action. Restart has no default shortcut or permanent
button: selecting it opens a quick-input confirmation, with Cancel focused.
It first verifies the running daemon's separate local-control
endpoint; an older daemon that lacks it leaves the active tab attached and
reports that restart is unavailable. Once accepted, it terminates every local
rmux session (including sessions opened by other apps) before both daemon
endpoints drain and a fresh daemon starts. Local workspace entries become
missing rather than being silently removed.
Remote tabs and their SSH attachments are unrelated and remain intact. It
cannot preserve daemon-owned PTYs, has no remote `ctl` equivalent, and is not a
version-mismatch escape hatch: protocol upgrades must remain compatible. Default
terminal shortcuts are:

- new shell: `Cmd/Ctrl-Shift-N`
- new tab in the current shell-reported directory: `Cmd-T` on macOS or
  `Ctrl-Shift-T` on Windows/Linux
- detach active tab: `Cmd-W` on macOS or `Ctrl-Shift-W` on Windows/Linux
- close active session after confirmation: `Cmd-E` on macOS or `Ctrl-Shift-E`
  on Windows/Linux
- next tab: `Cmd/Ctrl-Shift-]`
- previous tab: `Cmd/Ctrl-Shift-[`

The close shortcut opens a quick-input confirmation with **Cancel** focused.
Press the close shortcut again (`Cmd-E` on macOS, `Ctrl-Shift-E` on Windows/Linux)
or choose **Terminate session** to terminate the session named in the prompt.
Press `Esc` to cancel. Other commands remain blocked while confirmation is open.

**New Shell**, from the sidebar, command palette, or `Cmd/Ctrl-Shift-N`, uses
the same overlay: choose a host (Local is first/default), then enter an optional
working directory. Blank means that host's home directory. Back preserves the
directory draft, and Escape cancels before submission without contacting a
host. Once creation starts, dismissal is disabled until it finishes because
the backend may already have created a persistent shell. Errors remain inline
for correction/retry; a successful creation is saved and opened as before.
Post-creation save or attachment failures preserve the existing shell and use
workspace/session recovery rather than inviting duplicate creation.

These are the defaults. **Configure Keyboard Shortcuts** in the palette opens
quick input: select a command and enter a combination such as
`Primary+Shift+Y` (`Primary` means Cmd on macOS and Ctrl elsewhere). Blank removes
the binding; `default` restores it. Commands without defaults can also be bound.
The close command's configured shortcut also confirms its own close dialog.
Dialog accept/cancel/back are scoped commands; accept and back have no default
shortcut. Ordinary Enter still submits a form or activates the focused button,
so Enter on a confirmation's initially focused Cancel button remains safe.

Overrides persist separately from workspace/session data in `keybindings.json`
under the native app configuration directory (the shortcut picker shows its
exact path). Example:

```json
{
  "schema_version": 1,
  "overrides": [
    {
      "command_id": "session.close",
      "keybinding": { "code": "KeyY", "primary": true, "shift": true }
    },
    { "command_id": "tab.new_shell_here", "keybinding": null }
  ]
}
```

Use **Reload Keyboard Shortcuts** after editing the file externally. Invalid,
conflicting, or unknown bindings leave the last valid keymap active and show an
error; the file is not overwritten automatically. Saves reject concurrent edits.
This version supports one single-keystroke combination per command, not chords
or arbitrary `when` expressions. Conflicts must be resolved by unbinding the
other command first. Unmodified typing keys cannot be assigned app-wide.

Sidebar, tab, toolbar, palette, dialog, and native-menu actions use the same
dispatcher, with explicit session/host targets and shared availability checks.
Native Command-modified accelerators and displayed labels derive from the
resolved keymap; unmodified dialog and Alt/function keys use the webview adapter.
Shortcuts are local to the focused app. Text editing, focus/list navigation,
and raw xterm/PTY input remain widget behavior. Standard native editing/window
commands (including Cmd-Q) and emergency reload after a renderer crash remain
platform/recovery operations, outside the configurable app command registry.

## Visual preview

The workbench uses a compact activity bar for Sessions, Tasks, and Ports, a
collapsible host/session tree, and shared tab and command styling. Host and
session row actions appear on hover or keyboard focus. The keyboard icon at the
bottom of the activity bar opens shortcut configuration. Closing a tab keeps its
session running; **Terminate session** is the separate destructive action.

For a browser preview with sample data, run `pnpm exec vite --host 127.0.0.1`
and open `http://127.0.0.1:1430/preview.html`. This development-only entry renders
the actual UI using in-memory Tauri mocks. It cannot execute terminal commands
or open SSH connections. See [preview details](dev/README.md).

## Verify

```sh
pnpm check
pnpm test
pnpm build
cargo test -p rmux-app
```

An opt-in backend integration test covers remote create, list, attach, and
kill through the same command functions invoked by Tauri:

```sh
RMUX_TEST_SSH_TARGET=rmux-docker cargo test -p rmux-app \
  commands::tests::creates_lists_attaches_and_kills_a_session_over_ssh \
  -- --ignored --exact
```

The SSH prompt bridge also has opt-in tests for the built helper binary and
the local test container at `127.0.0.1:2222`. The host-key test uses a temporary
known-hosts file; provide the fingerprint independently inspected in the
container, never one learned from an unverified connection:

```sh
cargo build -p rmux-app
RMUX_TEST_ASKPASS_PROGRAM=/absolute/path/to/target/debug/rmux-app \
RMUX_TEST_SSH_IDENTITY=/absolute/path/to/private-key \
RMUX_TEST_SSH_FINGERPRINT=SHA256:verified-container-fingerprint \
cargo test -p rmux-app ssh_auth::tests -- --ignored
```

The GUI never resizes an existing PTY merely because it was selected. **Resize
with window** explicitly acquires layout ownership and then keeps the PTY grid
matched to this window; turning it off releases layout ownership. Sessions
created by this GUI start in resize-with-window mode because this window
establishes their initial layout. Authoritative geometry changes update the
active session's size in the sidebar immediately; inactive rows refresh from
the daemon when the session list is refreshed.
