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

The app may also use the path in `RMUXD_BIN`. Detaching or closing the app does
not kill a session.

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
establishes their initial layout.
