# ctmux TUI

A terminal client for the same sessions and server-owned split views used by
the desktop app. Sessions are the navigation layer; there are no nested windows
or tab groups.

## Run

The main entry point is `ctmux`; it embeds this TUI as a library.

```sh
cargo build -p ctmux-cli -p ctmuxd
cargo run -p ctmux-cli                         # create a session and open the TUI
cargo run -p ctmux-cli -- new -s work
cargo run -p ctmux-cli -- new -As work         # attach if it exists, otherwise create
cargo run -p ctmux-cli -- new -ds background   # create detached, including in scripts
cargo run -p ctmux-cli -- attach -t work
cargo run -p ctmux-cli -- attach -rt work      # read-only
cargo run -p ctmux-cli -- ls
cargo run -p ctmux-cli -- kill-session -t work
```

`new-session`, `attach-session`, and `list-sessions` accept the short aliases
`new`, `attach`/`a`, and `ls`/`list`. Existing `attach NAME`, `kill NAME`,
and `new --name NAME` syntax is retained. `attach` without a target selects the
newest running session; it never creates one. `new -A` requires `-s NAME`.

Use `-c DIRECTORY` with `new` to set its initial directory, and
`-- PROGRAM ARGS...` to run a specific program. Global `-S PATH`/`--socket PATH`
selects the local endpoint; `--prefix Ctrl+a` changes the TUI prefix.

Install `ctmuxd` beside `ctmux`, or set `CTMUXD_BIN`. They must use matching
protocol versions; the client does not restart or upgrade an existing daemon.

```sh
cargo install --path ctmux/daemon
cargo install --path ctmux/cli
```

The standalone `ctmux-tui [SESSION]` launcher remains available. It retains its
original behavior: open the first session, or create one when none exist.
`ctmux attach --raw NAME` (and `attach --from SEQUENCE`) uses the original
single-terminal presenter with Ctrl+] detach. `ctl ctmux` also retains its
transport-based presenter and detached creation behavior, including over SSH.
`ctl shell` uses this same TUI over its selected local or SSH connection, with
the banner, pane controls, reconnect handling, and history browser. Use
**Ctrl+B d** to detach. `ctl shell --plain` opens an ordinary shell.

Migration: standalone `ctmux new` now attaches by default. Scripts that used it
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
| Page Up | Open history one page back |
| `]` | Paste the local copy buffer into the active pane |
| `I` | Take or release the active pane's input lease |
| `R` | Take or release the view's resize lease |
| `x` | Terminate the active pane; `y` confirms |
| `d` | Detach and exit; sessions keep running |
| `?` | Help |
| Esc | Cancel prefix |

Sessions take the place of tmux windows for `c`, `n`, `p`, and `w`; ctmux
has no extra window layer. Uppercase `I` and `R` are ctmux-specific lease controls.

Press the prefix twice to send it to the active pane. Other keys, including
Ctrl+C, are forwarded to the active PTY. The TUI supports conventional xterm
keys, modified arrows, function keys, Unicode input, and bracketed paste.

## Scrollback and copy mode

The bottom status row shows the focused pane's connection and history state:
`connected` or `reconnecting`, and `history syncing`, `history ready`, or
`history incomplete`. Ready means the retained window is available, not that
the remote keeps unlimited history. The row stays outside the PTY grid and
scrollback. Copy mode keeps its frozen history and selection while connection
status updates; reopening copy mode picks up newly synchronized history.

Press **Ctrl+B [** to inspect a frozen snapshot of the active pane's visible
screen and its retained scrollback. The snapshot stays inside that pane; other
panes remain visible and continue updating. Each pane keeps its own copy mode,
so prefix commands can change focus without discarding a selection.
It includes history supplied by the daemon on attachment/reconnection, plus up
to 2,000 locally retained scrollback rows since the last checkpoint. Copy mode
also works in a read-only attachment. If local retention evicts rows, the older
checkpoint prefix is dropped too, keeping the displayed history contiguous.

- Arrows or `h/j/k/l` move; Page Up/Down move a page.
- Mouse wheel or trackpad scrolling focuses the pane under the pointer and opens
  history when the application has not requested mouse reporting, moving five rows per event. Scrolling back to the bottom returns to
  live output unless a selection is active. Keyboard-opened history stays open.
  Shift+Page Up also opens history; ordinary Page Up in the live view goes to
  the running program. Esc or `q` returns to live output.
- `g` / `G` jump to the first / last line; Home/End or `0` / `$` move within a line.
- `/` searches forward, `?` backward; Enter runs a case-sensitive literal search.
  `n` repeats and `N` reverses direction, wrapping at the history boundary.
- Emacs copy keys follow tmux: Ctrl+Space starts a selection, Ctrl+G clears it,
  Alt+W or Ctrl+W copies and exits, and Ctrl+C cancels. Ctrl+B/F/P/N moves the
  cursor (Ctrl+B remains the default prefix; a configurable prefix frees it),
  Ctrl+A/E moves within a line, Alt+V pages up, and Ctrl+V or Space pages
  down. Alt+`<`/`>` jumps to the top/bottom; Ctrl+R/S opens backward/forward search.
- Existing vi shortcuts remain: `h/j/k/l`, `g/G`, `v` to select, and `y` or Enter
  to copy. Space follows the Emacs default and pages down.
- Esc or `q` returns to the live view. Esc while entering a search cancels the prompt.

Live-pane snapshots preserve physical screen rows; copying and searching
join soft wraps without adding newlines. Full-screen applications show their
active alternate buffer without exposing the hidden shell screen. Archived logical lines scroll
horizontally with the cursor. Click a live pane to focus it, or drag with the
left button to select and copy on release. A drag stays with the pane where it
started, even when the pointer crosses a divider. Dividers and the fixed status row are excluded
from pane hit testing, including when a shared canvas is larger than the window.
New output, reconnects, and resizing do not change the frozen selection.
Keyboard input and host paste are consumed locally in the focused copy pane;
prefix commands remain available to focus other panes, split, or detach.

Applications that request mouse reporting receive pane-relative button, drag,
wheel, or motion events according to their requested tracking mode. These
reports require the pane's input lease. Shift requests local selection/history
instead when the host terminal delivers the modified mouse event. Read-only
clients always browse locally. Mouse and bracketed-paste modes survive daemon
checkpoints and reconnects. Pixel-coordinate mouse reporting is not supported.

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

Pane border dragging, zoom and resizing commands, extended keyboard protocols,
and a command prompt remain unimplemented. Rendering shares the
daemon's `avt` terminal emulation capabilities; it is not full tmux feature parity.

## Validate

```sh
cargo test -p ctmux-tui
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

Dismissing an ended or confirmed missing session saves a client-local archive
for seven days. Archives retain the session identity, per-pane end message,
and text available in this client's buffers. Output the client never received
cannot be recovered. Transport failures alone do not archive a session.

Use `ctmux archives` to list local TUI archives and `ctmux archive SESSION_ID` to
browse one read-only. **Ctrl+B A** opens the archive list; select a session and
pane, then press Enter to inspect/search/copy its output. No daemon connection
is needed for archives. The desktop **Archived** browser uses its own local store.

Set `CTMUX_ARCHIVE_DIRECTORY` to override the client storage directory.

Archives live in `~/.tokn/ctl/ctmux/tui/archives/`
(or `~/.tokn/ctl/ctmux/desktop/archives/` for desktop text archives). They expire
after seven days and are removed when listing the store. No daemon flags or protocol changes are
needed. `ctl ctmux archives` also lists this client's TUI archive metadata.
