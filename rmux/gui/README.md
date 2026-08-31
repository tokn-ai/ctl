# rmux desktop

The local desktop client for daemon-owned `rmux` terminal sessions. It uses
Tauri 2, React/TypeScript, and xterm.js.

## Develop

From the repository root, build the daemon so the development app can find it
beside its own Cargo binary, then start Tauri:

```sh
cargo build -p rmuxd
cd rmux/gui
pnpm install
pnpm tauri dev
```

The app may also use the path in `RMUXD_BIN`. GUI-created shells receive an
automatic `session-N` name. **Disconnect** closes the active tab and detaches
its view while leaving the shell running. **Close** is deliberately destructive:
after confirmation it terminates the session for all clients. Closing the app
itself only detaches its active view.

The desktop normally has one native window and one WebView. Selecting a daemon
session opens it as a local tab, but only the active tab holds an attachment.
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
rmux session before both daemon endpoints drain and a fresh daemon starts. It
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
cargo test -p rmux-gui
```

The GUI never resizes an existing PTY merely because it was selected. **Resize
with window** explicitly acquires layout ownership and then keeps the PTY grid
matched to this window; turning it off releases layout ownership. Sessions
created by this GUI start in resize-with-window mode because this window
establishes their initial layout. Authoritative geometry changes update the
active session's size in the sidebar immediately; inactive rows refresh from
the daemon when the session list is refreshed.
