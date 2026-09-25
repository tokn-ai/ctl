# rmux TUI

A terminal client for the same sessions and server-owned split views used by
the desktop app. Sessions are the navigation layer; there are no nested windows
or tab groups.

## Run

Build the client and its local daemon together:

```sh
cargo build -p rmux-tui -p rmuxd
cargo run -p rmux-tui
cargo run -p rmux-tui -- my-session
cargo run -p rmux-tui -- --read-only my-session
cargo run -p rmux-tui -- --prefix Ctrl+a --socket /path/to/rmux.sock
```

With no session argument, the client opens the first running session, or creates
a shell if none exist. Read-only mode never creates a shell. A named session must
already exist. The daemon is auto-started through the existing local connector;
install `rmuxd` beside `rmux-tui`, or set `RMUXD_BIN`.

```sh
cargo install --path rmux/daemon
cargo install --path apps/tui
```

This first version connects locally. To use a remote daemon, SSH to the machine
and run `rmux-tui` there. Protocol versions must match; the TUI does not restart
or upgrade an existing daemon.

## Keys

The default prefix is **Ctrl+B**. Change it with `--prefix Ctrl+a` or
`--prefix Alt+a`. Prefix settings are local to this invocation.

| After prefix | Action |
| --- | --- |
| `%` or `v` | Split right |
| `"` or `s` | Split below |
| Arrow keys | Focus an adjacent pane |
| `c` | Create and select a session |
| `n` / `p` | Next / previous session |
| `w` | Session picker; arrows select, Enter opens, Esc cancels |
| `i` | Take or release the active pane's input lease |
| `r` | Take or release the view's resize lease |
| `x` | Terminate the active pane; `y` confirms |
| `d` | Detach and exit; sessions keep running |
| `?` | Help |
| Esc | Cancel prefix |

Press the prefix twice to send it to the active pane. Other keys, including
Ctrl+C, are forwarded to the active PTY. The TUI supports conventional xterm
keys, modified arrows, function keys, Unicode input, and bracketed paste.

## Shared views and rendering

The bottom row is a session/status bar; the remaining terminal area is the
requested view canvas. One attachment owns the view-wide resize lease. Other
clients preserve the server's dimensions, clipping their viewport and following
the active pane's cursor when the shared canvas is larger than their terminal.
Input ownership is independent for each pane. Lease requests never displace
another client.

Each pane has its own bounded VT emulator. The renderer uses authoritative pane
rectangles and the server's reserved separator cells, without taking rows or
columns away from PTYs for borders. It handles colors, attributes, wide cells,
alternate screens, cursor visibility and cursor-key mode. Daemon checkpoints
reset the emulator before replay, and split UTF-8 bytes survive chunk boundaries.

An attachment controller handles heartbeats, backpressure and ordered rendering
acknowledgements. A disconnected pane reconnects using its attachment token and
a fresh checkpoint; if the token expired, it opens a new attachment. Topology
and session lists refresh every two seconds and after local changes. Detaching
releases leases without terminating the session. Normal exit, errors, and Unix
termination/hangup signals restore the host terminal mode and alternate screen.

The first version does not include copy/scrollback mode, mouse input, pane
dragging, extended keyboard protocols, or a command prompt. Rendering shares the
daemon's `avt` terminal emulation capabilities; it is not full tmux feature parity.

## Validate

```sh
cargo test -p rmux-tui
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The Unix integration test uses a temporary daemon and real PTYs to check
separate pane output, shared sizing, read-only viewing, reconnect, and lease
release on detach. It needs permission to bind a local Unix socket.
