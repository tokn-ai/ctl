# rmux TUI

A terminal client for the same sessions and server-owned split views used by
the desktop app. Sessions are the navigation layer; there are no nested windows
or tab groups.

## Run

The main entry point is `rmux`; it embeds this TUI as a library.

```sh
cargo build -p rmux -p rmuxd
cargo run -p rmux                         # create a session and open the TUI
cargo run -p rmux -- new -s work
cargo run -p rmux -- new -As work         # attach if it exists, otherwise create
cargo run -p rmux -- new -ds background   # create detached, including in scripts
cargo run -p rmux -- attach -t work
cargo run -p rmux -- attach -rt work      # read-only
cargo run -p rmux -- ls
cargo run -p rmux -- kill-session -t work
```

`new-session`, `attach-session`, and `list-sessions` accept the short aliases
`new`, `attach`/`a`, and `ls`/`list`. Existing `attach NAME`, `kill NAME`,
and `new --name NAME` syntax is retained. `attach` without a target selects the
newest running session; it never creates one. `new -A` requires `-s NAME`.

Use `-c DIRECTORY` with `new` to set its initial directory, and
`-- PROGRAM ARGS...` to run a specific program. Global `-S PATH`/`--socket PATH`
selects the local endpoint; `--prefix Ctrl+a` changes the TUI prefix.

Install `rmuxd` beside `rmux`, or set `RMUXD_BIN`. They must use matching
protocol versions; the client does not restart or upgrade an existing daemon.

```sh
cargo install --path rmux/daemon
cargo install --path rmux/cli
```

The standalone `rmux-tui [SESSION]` launcher remains available. It retains its
original behavior: open the first session, or create one when none exist.
`rmux attach --raw NAME` (and `attach --from SEQUENCE`) uses the original
single-terminal presenter with Ctrl+] detach. `ctl rmux` also retains its
transport-based presenter and detached creation behavior, including over SSH.
For the local TUI on a remote machine, SSH there and run `rmux`.

Migration: standalone `rmux new` now attaches by default. Scripts that used it
to create background sessions must add `-d`.

## Keys

The default prefix is **Ctrl+B**. Change it with `--prefix Ctrl+a` or
`--prefix Alt+a`. Prefix settings are local to this invocation.

| After prefix | Action |
| --- | --- |
| `%` | Split right |
| `"` | Split below |
| Arrow keys | Focus an adjacent pane |
| `o` | Cycle to the next pane |
| `c` | Create and select a session |
| `n` / `p` | Next / previous session |
| `s` / `w` | Session picker; arrows select, Enter opens, Esc cancels |
| `r` | Redraw the terminal |
| `I` | Take or release the active pane's input lease |
| `R` | Take or release the view's resize lease |
| `x` | Terminate the active pane; `y` confirms |
| `d` | Detach and exit; sessions keep running |
| `?` | Help |
| Esc | Cancel prefix |

Sessions take the place of tmux windows for `c`, `n`, `p`, and `w`; rmux
has no extra window layer. Uppercase `I` and `R` are rmux-specific lease controls.

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
