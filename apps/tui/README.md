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
| Arrow keys | Focus an adjacent pane; repeat without a prefix for 500 ms |
| Ctrl + Arrow | Move the adjacent divider by one cell; repeat for 500 ms |
| Alt + Arrow | Move the adjacent divider by five cells; repeat for 500 ms |
| `o` | Cycle to the next pane |
| `z` | Zoom/unzoom the focused pane; requires the view resize lease |
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

After a prefix plus an arrow, further focus or resize arrows repeat
when pressed within 500 ms of the previous repeatable key. Any other key ends
repetition and follows ordinary input handling, including copy mode keys.
Commands such as detach and split always require a fresh prefix. Ctrl, Alt,
and Shift remain part of the binding: Ctrl/Alt arrows resize, while Shift arrows
have no prefix binding. Outside a prefix or repeat window, modified arrows go
to the active PTY.

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

**Ctrl+B z** toggles shared pane zoom. The daemon resizes that pane to the full
canvas above the status row and preserves the split layout. Hidden panes keep
running with their attachments, input leases, and copy selections. Unzoom restores
the split geometry for the current canvas. Focus navigation unzooms first; a
client without the resize lease cannot change shared zoom. Splitting or changing
the layout clears zoom, and exiting the zoomed pane clears zoom. Its final
screen remains visible until dismissed, as with other ended panes.
Zoom survives detach and reconnect, and the status row shows `ZOOM`.

Zoom supports negotiated ctmux contracts `1.1.15`, `1.1.16`, and `1.1.17`.
Older daemons remain usable with their existing split controls; attempting zoom
reports that it is unavailable.

**Ctrl+B Ctrl+Arrow** moves a divider by one cell; **Ctrl+B Alt+Arrow** moves it
by five. The daemon chooses the nearest split in that direction's axis, using
the divider after the focused child or the preceding divider for the last child.
Left/up moves the divider left/up, and right/down moves it right/down. This can
shrink the focused pane when it is the last child. Subtree minimum sizes limit
movement; a divider at its limit stays put.

Resizing requires the view resize lease and negotiated ctmux contract `1.1.16`
or `1.1.17`.
A successful movement unzooms the view and saves its new proportions. The TUI
and desktop see the same rectangles; canvas changes, detach, and reconnect
retain the proportions. Daemon restart still ends these in-memory views.
Clients selecting an earlier contract can view unequal panes and rearrange
compatible layouts without resetting proportions, but cannot resize dividers.
Desktop mouse dragging requires `1.1.17`; the TUI receives the same confirmed
geometry. TUI resizing uses the keyboard bindings above. If this
client does not own resize, **Ctrl+B R** requests the available view lease.

Each pane has its own bounded VT emulator. The renderer uses authoritative pane
rectangles and the server's reserved separator cells, without taking rows or
columns away from PTYs for borders. It handles colors, attributes, wide cells,
alternate screens, cursor visibility and cursor-key mode. Daemon checkpoints
reset the emulator before replay, and split UTF-8 bytes survive chunk boundaries.

An attachment controller handles heartbeats, backpressure and ordered rendering
acknowledgements. A disconnected pane reconnects using its attachment token and
a fresh checkpoint; if the token expired, it opens a new attachment with the
user's current input and layout lease preferences. Releasing a lease remains in
effect across reconnects. The last screen stays visible until the replacement
attachment supplies its checkpoint, and copy-mode selections remain frozen.

Reconnects and periodic topology/session refreshes run beside rendering and
input handling, so stalled background requests do not block local controls.
Switching sessions or detaching cancels pending recovery work. Topology and session lists
refresh every two seconds and after local changes. While the TUI owns the host
terminal, `ctl` connection preparation displays errors through the TUI rather
than printing progress or asking for authentication in the terminal. If SSH
authentication is required, detach and reconnect to answer the prompt. Detaching
releases leases without terminating the session. Normal exit, errors, and Unix
termination/hangup signals restore the host terminal mode and alternate screen.

TUI pane border dragging, extended keyboard protocols,
and a command prompt remain unimplemented. Rendering shares the
daemon's `avt` terminal emulation capabilities; it is not full tmux feature parity.

## Validate

```sh
cargo test -p ctmux-tui
cargo test -p ctmux-tui --test terminal_process
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Fast tests check the key state machine, pane models, and renderer. Daemon-backed
handler tests check shared sizing, read-only viewing, reconnect, and input leases.

`terminal_process` launches the real `ctmux-tui` binary inside a host PTY on
Linux and macOS. It sends terminal bytes through crossterm and checks the rendered
screen, covering modifier handling, repeated pane navigation, paste, copy mode,
mouse scrolling, the bottom status row, shared pane resizing, detach, and host terminal loss.
Reconnect cases cut live streams repeatedly, stall metadata requests, expire
resume tokens, and check controls, actual shell input, and released leases.
These tests need permission to bind local Unix sockets and open PTYs.

The reusable test framework lives in `tests/support`: `TestDaemon` owns an
isolated daemon and controlled shell fixtures; `Tui` drives the host PTY; `Screen`
is a snapshot parsed from captured ANSI output. `Tui::spawn` accepts a command for
testing other launchers. `TestProxy` cuts or holds selected connections at wire
barriers while forwarding the others. The `ctl-cli` tests reuse these fixtures
to exercise SSH authentication retries inside the real TUI. New cases belong in
`tests/cases` and are registered in
`terminal_process.rs`.

Wait for observable screen conditions with `wait_screen` rather than fixed
startup delays. Failures report child status, the visible screen, and a bounded
raw transcript. Each fixture has private sockets and storage; teardown closes
the TUI and cooperatively stops its daemon and shell processes, including during
test unwinding. Exact repeat deadlines stay in the fast key-state tests; process
tests check repetition with batched keys and expiry with scheduling slack.

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
