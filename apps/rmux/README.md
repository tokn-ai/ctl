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
**This app only** stores the same non-secret settings in WebView local storage
and supplies them to OpenSSH as fixed arguments. Passwords, private-key
contents, arbitrary options, forwarding, and remote commands are never stored.
The app remembers the active alias in either case so it can restore the mixed
host list on launch; config-backed targets keep only that alias locally.
Existing destination-only app storage migrates automatically.

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
default for a new shell. The sidebar mixes sessions from every selected target
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
session for all clients. Closing the app itself only detaches its active view.

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
`Restart rmuxd` maintenance action. Restart has no shortcut or permanent
button: selecting it opens a quick-input confirmation, with Cancel focused.
It first verifies the running daemon's separate local-control
endpoint; an older daemon that lacks it leaves the active tab attached and
reports that restart is unavailable. Once accepted, it terminates every local
rmux session before both daemon endpoints drain and a fresh daemon starts.
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
Choose **Close session** to terminate it, or `Esc` to cancel. Repeating a
shortcut while a prompt is open cannot bypass confirmation. New-shell input
remains in the sidebar for now.

These shortcuts are local to the focused app. Other key combinations continue
to xterm and the PTY unchanged. The macOS application menu owns `Cmd-W` and
`Cmd-E` so native window-close handling cannot race the WebView; `Cmd-Q` keeps
its standard application-quit behavior.

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
