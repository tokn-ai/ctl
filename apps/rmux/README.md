# rmux app

The desktop client for local and SSH-connected daemon-owned `rmux` terminal
sessions. It uses Tauri 2, React/TypeScript, and xterm.js.

## Develop

From the repository root, build the daemon so the development app can find it
beside its own Cargo binary, then start Tauri:

```sh
cargo build -p rmuxd
cd apps/rmux
pnpm install
pnpm tauri dev
```

The app may also use the path in `RMUXD_BIN`. Open **+ Host** to activate a
concrete alias discovered from `~/.ssh/config` (including its `Include` files)
or enter `[user@]hostname[:port]`, then a name, then choose SSH config/agent,
an identity-file path, or password/interactive authentication. These steps use
the same quick-input overlay as the command palette. OpenSSH requests any
required host-key confirmation, password, passphrase, or interactive response
there. `ctld` must already be on the remote `PATH`; custom command paths are
not supported. After a successful connection, choose where to save the host.
**OpenSSH config**
writes a clearly marked `Host` block to `~/.ssh/config`, making the alias
reusable by `ssh` and `ctl`; an existing unmanaged alias is never overwritten.
**This app only** stores the same non-secret settings in the native workspace file
and supplies them to OpenSSH as fixed arguments. Passwords, private-key
contents, arbitrary options, forwarding, and remote commands are never stored.
The app remembers the active alias in either case so it can restore the mixed
host list on launch; config-backed targets keep only that alias locally.
Existing WebView host settings migrate automatically after a successful disk write.

The workspace file lives in Tauri's app-data directory as `workspace.json`.
It remembers known sessions, cached paths, tab order, and selection. Startup
restores those entries as unverified and automatically connects the selected
local tab. Remote hosts stay disconnected until explicitly opened; **Connect
host** resumes that host's selected tab, or its first open tab if another host
was selected. No daemon inventory is discovered automatically. Use **Add
existing session** in the sidebar or command palette to discover one host's
inventory and explicitly remember sessions without attaching. **Refresh Known
Sessions** inspects only remembered IDs; it does not adopt other apps' sessions.
Old sessions were never saved, so the first migration requires explicit import.
See [workspace persistence](../../docs/rmux-workspace.md) for recovery and tests.

The identity-file input suggests candidate files from the top level of
`~/.ssh`. Type to filter, use the arrow keys and Enter, or click a file. Manual
paths remain available, including when discovery fails. Rust lists names and
metadata only: it does not open key contents. Public keys, common SSH support
files, backups, and directories are omitted; symlinks to regular files are
supported. Suggestions are not proof that a file is a valid private key—OpenSSH
validates the selected identity when connecting.

Wildcard and negated `Host` patterns are not destinations and are omitted from
suggestions. Discovery only fills the picker: the app does not contact an SSH
host until it is explicitly selected. Local is always present and remains the
default for a new shell. The sidebar mixes remembered sessions from selected targets
and labels each row with its host. A failed host reports its own error while
last-known sessions from other targets remain usable.

SSH uses `ctl-core` and the system `ssh` executable with the fixed remote
command `exec ctld connect`; forwarding, agent access, X11, local commands,
and PTY allocation remain disabled. On macOS/Linux, a short-lived owner-only
Unix socket connects OpenSSH's askpass helper to the quick-input UI. Host-key
trust requires explicit confirmation and is managed by OpenSSH. Passwords and
key passphrases are cached only in native process memory, never saved to disk,
command arguments, environment variables, or logs; one-time responses are not
cached. Removing a host forgets its cached credentials. Click a host chip to
authenticate again after relaunch or failed credentials. Background connections
never open unsolicited prompts and time out after ten seconds; an explicit
interactive attempt allows up to three minutes and Escape cancels it. On other
platforms, preconfigured noninteractive SSH remains available.

GUI-created shells receive an automatic `session-N` name. **Disconnect**
removes an open tab while leaving its shell running. For the active tab it also
detaches the live view; inactive tabs have no live attachment to detach.
**Close** is deliberately destructive: after confirmation it terminates the
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
or choose **Close session** to terminate the session named in the prompt.
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
