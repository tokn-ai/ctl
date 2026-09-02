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
or enter a hostname/IP and optional alias, user, port, and identity-file path.
The next step chooses where the new definition is saved. **OpenSSH config**
writes a clearly marked `Host` block to `~/.ssh/config`, making the alias
reusable by `ssh` and `ctl`; an existing unmanaged alias is never overwritten.
**This app only** stores the same non-secret settings in WebView local storage
and supplies them to OpenSSH as fixed arguments. Passwords, private-key
contents, arbitrary options, forwarding, and remote commands are never stored.
The app remembers the active alias in either case so it can restore the mixed
host list on launch; config-backed targets keep only that alias locally.
Existing destination-only app storage migrates automatically.

Wildcard and negated `Host` patterns are not destinations and are omitted from
suggestions. Discovery only fills the picker: the app does not contact an SSH
host until it is explicitly selected. Local is always present and remains the
default for a new shell. The sidebar mixes sessions from every selected target
and labels each row with its host. A failed host reports its own error while
last-known sessions from other targets remain usable.

SSH uses `ctl-core` and the system `ssh` executable with the fixed remote
command `exec ctld connect`; forwarding, agent access, X11, local commands,
and PTY allocation remain disabled. Configure authentication and host trust
before connecting—the app does not implement password or host-key prompt UI.
Connection attempts are bounded to ten seconds.

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
button: selecting it in the palette changes it into a second confirmation
command. It first verifies the running daemon's separate local-control
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

The close shortcut first opens its confirmation with **Close** focused. Press
the same shortcut again to terminate the pending session, or `Esc` to cancel.

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

The GUI never resizes an existing PTY merely because it was selected. **Resize
with window** explicitly acquires layout ownership and then keeps the PTY grid
matched to this window; turning it off releases layout ownership. Sessions
created by this GUI start in resize-with-window mode because this window
establishes their initial layout. Authoritative geometry changes update the
active session's size in the sidebar immediately; inactive rows refresh from
the daemon when the session list is refreshed.
