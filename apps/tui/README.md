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
| `A` | Browse retained session archives |
| `[` | Browse history and select text in copy mode |
| `]` | Paste the local copy buffer into the active pane |
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

## Scrollback and copy mode

Press **Ctrl+B [** to inspect a frozen snapshot of the active pane's primary
screen and retained scrollback. The snapshot fills the terminal temporarily;
all panes continue processing and acknowledging output in the background.
It includes history supplied by the daemon on attachment/reconnection, plus up
to 2,000 locally retained scrollback rows since the last checkpoint. Copy mode
also works in a read-only attachment. If local retention evicts rows, the older
checkpoint prefix is dropped too, keeping the displayed history contiguous.

- Arrows or `h/j/k/l` move; Page Up/Down move a page.
- `g` / `G` jump to the first / last line; Home/End or `0` / `$` move within a line.
- `/` searches forward, `?` backward; Enter runs a case-sensitive literal search.
  `n` repeats and `N` reverses direction, wrapping at the history boundary.
- Space or `v` anchors a selection; Enter or `y` copies and exits.
- Esc or `q` returns to the live view. Esc while entering a search cancels the prompt.

Logical lines remain intact; long lines scroll horizontally with the cursor.
Selection adds newlines only between logical lines, without terminal padding.
New output, reconnects, and resizing do not change the frozen selection.
Keyboard input and host paste are consumed locally while copy mode is open.

Copied text is kept in this client's buffer. **Ctrl+B ]** pastes it using the
active pane's input lease and bracketed-paste setting. Copy also requests the
host clipboard through OSC 52; terminal support and permissions determine
whether that request succeeds (including over SSH). Selections above 100 KB
stay in the internal buffer without sending a clipboard request.

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

## Exited sessions and archives

An exited pane keeps its final output and exit code visible until you press a
key. That key dismisses only the ended pane; when the whole session has ended,
it exits the TUI attachment. A confirmed missing session behaves the same way.
Connection failures remain reconnectable and are not treated as confirmed exits.

Normal exits and explicit termination are archived on the daemon's host for
seven days by default. Archives retain the session identity, composition,
per-terminal exit reason and code, final checkpoint, and bounded history. They
survive daemon restarts and are separate from the running session list.

Use `rmux archives` to list archives and `rmux archive SESSION_ID` to browse one
read-only. Within the TUI, **Ctrl+B A** opens the archive list; choose a session,
then a terminal, and press Enter to inspect/search/copy its output. An archive
cannot accept input or revive a process. `ctl rmux archives` and
`ctl rmux archive SESSION_ID` expose archive metadata through the configured
transport. The desktop's Sessions sidebar has an **Archived** browser as well.

Configure retention with `rmuxd --archive-retention-days DAYS` and optionally
`--archive-directory DIRECTORY` when starting the daemon. The default endpoint
uses an endpoint-specific directory below the user's local data directory at
`rmux/archives/`. Explicit custom sockets default to a sibling `.archives`
directory so isolated daemons stay isolated. Expired archives are inaccessible
immediately, and are removed at startup, when listing, or during the daemon's
minute-by-minute cleanup. Cleanup resumes on the next start if the daemon is
not running. Retention changes apply to newly completed sessions.

The archive API requires protocol version 13 on clients and daemons.
