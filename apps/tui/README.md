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
| `;` | Return to the last focused pane; repeat to toggle |
| `q` | Show pane numbers for one second; `1`–`9` selects a pane |
| `{` / `}` | Swap the focused pane with the previous / next pane, wrapping |
| `!` | Move the focused pane into a new flat session and follow it |
| `z` | Zoom/unzoom the focused pane; requires the view resize lease |
| `c` | Create and select a session |
| `n` / `p` | Next / previous session |
| `l` / `L` | Return to the last session, restoring its remembered pane |
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
| `:` | Open the command prompt |
| Esc | Cancel prefix |

Sessions take the place of tmux windows for `c`, `n`, `p`, `l`, and `w`; ctmux
has no extra window layer. Uppercase `I` and `R` are ctmux-specific lease controls.

Pane numbers start at **1**, matching the status row. While numbers are shown,
digits select a pane and any other key or paste dismisses the overlay locally.
Use `select-pane -t NUMBER` in the command prompt for panes above 9. Numbers do
not resize PTYs or change history; ordinary geometry refreshes preserve the
displayed mapping, while changed membership, order, or zoom visibility cancels it.
Returning to a pane preserves its frozen copy selection. Returning to a session
reattaches normally and restores its remembered pane when it still exists.
Missing previous targets report an error without closing the current session.
The session picker keeps the highlighted session by ID during background refresh;
if it disappears, choose another session with the arrows before opening it.

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

## Command prompt

Press **Ctrl+B :** to enter a command in the existing bottom row. The pane grid
keeps its full height, and output and history synchronization continue. Prompt
input stays local, including while a pane is in copy mode or reconnecting.
Enter submits; Esc, Ctrl+C, or Ctrl+G cancels. Cancelling preserves the focused
pane and its frozen copy selection.

Left/Right, Home/End, Backspace, and Delete edit the line. Ctrl+A/E moves to the
start/end, Ctrl+B/F moves left/right, Ctrl+U/K clears before/after the cursor,
and Ctrl+W deletes the preceding word. Up/Down recalls commands from this
invocation's history; Tab completes a command name. Pasted text edits the line
without submitting it, and terminal control characters are filtered.

The first command set uses familiar tmux names and aliases:

| Command | Action |
| --- | --- |
| `split-window` / `splitw [-h\|-v]` | Split right with `-h`; default or `-v` splits below |
| `select-pane` / `selectp -L\|-R\|-U\|-D` | Focus an adjacent pane |
| `select-pane -t NUMBER` | Focus a one-based pane number in the current session |
| `last-pane` / `lastp`, or `select-pane -l` | Return to the last focused pane |
| `display-panes` / `displayp` | Show temporary pane numbers for selection |
| `resize-pane` / `resizep -L\|-R\|-U\|-D [N]` | Move the adjacent divider by `N` cells, default 1 |
| `resize-pane -Z` | Toggle shared pane zoom |
| `swap-pane` / `swapp -U\|-D [-d]` | Swap with the previous / next pane; `-d` keeps focus in the original slot |
| `break-pane` / `breakp [-d] [-n NAME]` | Move to a new flat session; `-d` stays in the original session |
| `new-session` / `new [-s NAME]` | Create and select a session |
| `switch-client` / `switchc -n\|-p\|-l\|-t NAME` | Next/previous/last session, or select an exact name or ID |
| `list-sessions` / `ls` | Open the session picker |
| `kill-pane` / `killp` | Open the existing pane termination confirmation; `y` confirms |
| `copy-mode [-u]` | Open history, optionally one page back |
| `paste-buffer` / `pasteb` | Paste the local copy buffer into the focused pane |
| `refresh-client` / `refresh` | Redraw the terminal |
| `detach-client` / `detach` | Detach this client |
| `list-keys` / `lsk` | Open help |
| `take-input` / `release-input` | Request or release the focused pane's input ownership |
| `take-resize` / `release-resize` | Request or release the view's resize ownership |

Ownership commands are idempotent: repeating `take-resize` keeps ownership,
and repeating `release-resize` keeps it released. Requests never displace another
client. Read-only attachments can use local navigation, copy, and help commands;
mutating commands retain their existing ownership and read-only checks.

For example, enter `resize-pane -R 5` or `new-session -s "build-logs"`.
Each submission accepts one command with literal quoted or escaped arguments.
Unsupported commands, flags, command sequences, and shell expansion syntax
report a local error. The prompt does not execute a shell fallback. Sessions
remain flat, as with the prefix controls above.

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

Zoom supports negotiated ctmux contracts `1.1.15` through `1.1.19`.
Older daemons remain usable with their existing split controls; attempting zoom
reports that it is unavailable.

**Ctrl+B Ctrl+Arrow** moves a divider by one cell; **Ctrl+B Alt+Arrow** moves it
by five. The daemon chooses the nearest split in that direction's axis, using
the divider after the focused child or the preceding divider for the last child.
Left/up moves the divider left/up, and right/down moves it right/down. This can
shrink the focused pane when it is the last child. Subtree minimum sizes limit
movement; a divider at its limit stays put.

Resizing requires the view resize lease and negotiated ctmux contract `1.1.16`
through `1.1.19`.
A successful movement unzooms the view and saves its new proportions. The TUI
and desktop see the same rectangles; canvas changes, detach, and reconnect
retain the proportions. Daemon restart still ends these in-memory views.
Clients selecting an earlier contract can view unequal panes and rearrange
compatible layouts without resetting proportions, but cannot resize dividers.
In both the TUI and desktop, drag a divider with the left mouse button to resize
its adjacent panes. Mouse dragging requires `1.1.17` and the view resize lease;
**Ctrl+B R** requests that lease when it is available. Dragging a divider keeps
the current pane focused and preserves its copy selection. A drag that starts
inside a pane continues to select text or report mouse input to its application.

The TUI waits for each resize confirmation and combines intervening pointer
movements, including the final mouseup position. Output and routine view refreshes
keep the drag active. Esc cancels remaining movement; disconnecting, changing the
view, losing resize ownership, or resizing the host terminal also ends the drag.
Already confirmed pane sizes remain in effect.

### Rearranging panes

**Ctrl+B {** and **Ctrl+B }** swap the focused terminal with its previous or
next neighbor in layout order, wrapping at either end. Split trees and slot
proportions stay fixed, so the desktop sees the same rearrangement. Focus follows
the terminal; `swap-pane -U -d` or `swap-pane -D -d` keeps focus in the original
slot instead. Swapping clears shared zoom.

**Ctrl+B !** moves the focused terminal into a new flat session and follows it.
`break-pane -n "build-logs"` names that session; `break-pane -d` leaves this client
in the original session. The running process, terminal identity, history, and
input attachment survive the move. Following the pane also retains its copy
selection. The client requests resize ownership in the session it keeps
displaying; another client's ownership
is never displaced. A session with one pane cannot be broken out.

These TUI commands require ctmux `1.1.19` and the source view's resize lease.
The daemon checks attachment ownership, view identity, revision, and pane
membership together. Older daemons remain usable and report pane moves unavailable;
no unsupported frame is sent. Existing CLI and desktop arrangement operations
retain their previous behavior.

Output, input, local controls, and resizing continue while a move awaits
confirmation.
Connected source observers remove a moved pane without treating it as an exit;
actual exited panes still retain their final output until dismissed.

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

## Modified keys

On Unix, `ctmux-tui` and `ctl shell` request unambiguous key events from host
terminals supporting the Kitty keyboard protocol, including shifted characters
reported by the active keyboard layout. Other terminals keep their
usual input. The request is local to the host alternate screen and is restored
on detach, errors, and handled termination signals.

With negotiated ctmux `1.1.18`, applications can request modified-key reporting
per pane using `CSI > 4 ; 1 m` or `CSI > 4 ; 2 m`. Mode 1 follows tmux's selective
policy and keeps familiar Ctrl-letter and Alt encodings. Mode 2 distinguishes
modified ordinary keys, including Ctrl+I versus Tab, Ctrl+Shift+A versus Ctrl+A,
and Ctrl+Enter versus Enter, using `CSI 27 ; modifier ; codepoint ~`.
Cursor and function keys retain their existing xterm sequences. Applications
query the level with `CSI ? 4 m` and reset it with `CSI > 4 ; 0 m`.
Checkpoint restoration preserves the requested mode across attach, resizing,
and reconnect. Shells that do not request it retain legacy input; older daemon
contracts also keep legacy encoding and do not answer the new mode query.

Host terminals must supply distinct events for these shortcuts to be distinct.
The TUI consumes prefix, command-prompt, and copy-mode keys locally, ignores
key releases, and does not implement full Kitty application reporting. GUI
modified-key reporting and xterm-only host negotiation remain future work.
Rendering shares the daemon's `avt` terminal emulation capabilities; it is not
full tmux feature parity.

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
Linux and macOS. It sends terminal bytes through the input decoder and checks the rendered
screen, covering modifier handling, repeated pane navigation, paste, copy mode,
mouse scrolling, the bottom status row, shared pane resizing, divider dragging,
detach, and host terminal loss. Drag cases check nested splits, held gestures
across view refreshes, frozen copy selections, mouseup targets, cancellation,
ownership, and actual PTY dimensions.
Command cases check the fixed footer, local editing and paste, frozen copy
selections, pane/session operations, ownership, errors, and reconnect controls.
Keyboard cases check application byte sequences, per-pane modes, checkpoint
restoration, reconnects, and local controls with enhanced host input.
Navigation cases check labelled pane selection, last-pane toggling with frozen
copy selections, last-session focus restoration, vanished targets, and session
picker identity during refresh.
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
