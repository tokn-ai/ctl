# ctmux published protocol 1.1.16

The protocol is independent of local IPC and future remote transport. Internal build
11 introduced length-prefixed JSON frames for debuggability. Each frame begins with a
four-byte unsigned big-endian payload length.

The maximum encoded frame size is 8 MiB.

### Internal build 7 attachment reconnect

Internal build 7 adds an opaque `attachment_token` to `attached` and a corresponding
`resume_attachment` request. The token rebinds a replacement transport to the
same logical attachment during a bounded reconnect grace, preserving its input
and layout leases. These earlier integer-only builds were unpublished development formats.

Internal build 8 adds renderer-applied presentation flow control. Internal build 9 pairs
every terminal checkpoint with a bounded normalized-history snapshot captured
at the same raw sequence.

Published contract `1.1.14` sends a small recent history tail with the live screen, followed by
client-requested byte pages from one pinned physical-history snapshot. Every
geometry transition and saved-history clear replaces the authoritative screen
and history manifest. History synchronization does not delay screen acknowledgement.

## Sessions, views, and terminals

Internal build 10 separates three identities:

- A **session** is the named root returned by `list_sessions`.
- Each session binds to one distinct, server-owned **view**.
- Each **terminal** owns its PTY, process, history, attachments, and input
  lease. The view owns the layout (resize) lease. It belongs to exactly one view.

Creation allocates a session, view, and initial terminal with distinct IDs.
`SessionInfo` includes `session_id`, `view_id`, and `terminal_id`. Its size and
output sequence describe the selected terminal, not an aggregate across a view.
Listing selects the first leaf in each view; attachment and shell inspection
select the requested terminal. The root name and creation time remain stable
when terminals move or the first terminal exits.

`attach_session`, `resume_attachment`, and `get_shell_state` accept a root name,
root ID, or terminal ID. A root selector opens the first terminal in layout order.
Clients must pin subsequent reconnects to the returned `terminal_id`, because
layout order and ownership may change. Output and input ownership remain scoped to that
terminal; layout ownership spans its entire view. The historical `session_ended` stream event indicates the attached
terminal's exit; other terminals in the root may still be running.

Internal build 11 makes `resize` and the layout lease view-wide. An attachment to any
member terminal may acquire the one layout lease; input leases remain independent.
The resize owner supplies the full canvas size. The daemon allocates integer cell
rectangles and resizes every member PTY, including hidden tab groups. Horizontal
splits divide columns, vertical splits divide rows; dividers consume one cell.
Remainder cells go to earlier children. The minimum is two columns and one row
per terminal. Resize requests smaller than the layout minimum use the minimum
canvas, which smaller clients can scroll. Splits that cannot fit are rejected.

`panes` entries contain `terminal_id`, `left`, `top`, `columns`, and `rows`.
Their coordinates are relative to `canvas_size`. Tab children share bounds;
clients retain their own selected tab and active pane. Observers display the
shared grid through a viewport and must not resize individual PTYs to fit.
Desktop and TUI clients can render the same coordinates without pixel-dependent
layout calculations. Geometry stream events continue to describe each PTY's own
allocated size, while `get_view` supplies the complete canvas geometry.

One-shot topology requests are:

| Request | Effect |
| --- | --- |
| `get_view { session }` | Return the root's complete `view_snapshot` |
| `split_terminal { terminal_id, axis, command, working_directory, terminal_size }` | Create a terminal beside the target in the same view |
| `update_view { session, expected_revision, layout }` | Replace arrangement with exactly the same terminal membership; reject stale revisions |
| `promote_terminal { terminal_id, name }` | Move one of a root's multiple terminals to a new session/view |
| `merge_sessions { source, destination }` | Join destination and source layouts in a horizontal split; remove source root/view |
| `kill_terminal { terminal_id }` | Terminate only that terminal |
| `kill_session { session }` | Terminate every terminal owned by the root; prevent new splits or moves into it |

Successful view operations return `view_snapshot { view }`; terminal termination
returns `success`. A snapshot includes `session_id`, `session_name`, `view_id`,
`revision`, `layout`, `canvas_size`, cell-coordinate `panes`, and terminal metadata. Revisions increase on changes to
layout, canvas size, or membership, including exit. Layout nodes use `kind`:

```json
{
  "kind": "split",
  "axis": "horizontal",
  "children": [
    { "kind": "terminal", "terminal_id": "terminal-a" },
    { "kind": "terminal", "terminal_id": "terminal-b" }
  ]
}
```

`horizontal` places children side by side and `vertical` stacks them.
Splits without weights divide space equally. Contract `1.1.16` adds optional
positive `weights`, one per child, for proportional allocation (see below).
Sessions provide tab-like navigation; views contain
only terminals and splits. Legacy `tabs` input decodes recursively into horizontal
splits, preserving terminal IDs and child order. The server
validates unique and complete membership, at most 64 terminals, and at most 16
levels of nesting. Pane focus is client-local; layout and membership are
server-owned. Membership transfers are atomic under the registry lock and keep
terminal IDs, processes, history, and existing attachments intact.

Exiting terminals are removed from their view; redundant one-child groups
collapse. The last exit removes the session and view, retaining the existing
daemon idle-exit behavior. Layouts survive client disconnects, but like PTYs,
do not survive daemon restart. Task-managed roots retain one terminal and reject
splits and transfers so task lifecycle ownership remains unambiguous.

Internal build 12 removed tabbed views; build 13 adopted the ctmux checkpoint/history
format names. Published `1.0.13` is the first contract. Handshakes negotiate the
highest explicit shared contract, with advertisements retained separately from
the selection. See [versioning policy](protocol-versioning.md). Unpublished older
daemons need replacement; this does not migrate their running sessions.

## Connection lifecycle

Every connection starts with `handshake { protocol, client_name, client_version }`,
where `protocol` advertises the internal build, latest published version, and
explicit `supported_versions` set. The daemon replies with
`handshake_accepted` or a structured error. One command follows a successful
handshake.

Most commands receive one response and close. `attach_session` creates a
logical attachment and changes the connection into a bidirectional stream.
`resume_attachment` rebinds a replacement stream to an existing logical
attachment:

- daemon to client: `attached` (including a complete `shell_state` snapshot),
  an optional checkpoint/history pair, replayed `output`, then live `output`,
  replacing `checkpoint`, requested `history_page`, and `shell_state_changed`;
- client to daemon: `input`, `resize`, lease acquire/release, `heartbeat`,
  `presentation_applied`, `history_request`, `request_checkpoint`, or `detach`;
- daemon to client: `heartbeat_ack`, `detached` after an explicit detach is
  processed, and `session_ended` when the child exits.

Explicit `detach` is complete after the client receives `detached`; the daemon
then closes the logical attachment immediately. Transport loss instead
suspends it for the negotiated reconnect grace and never kills the session.
Creating a session carries its initial working directory separately from the
optional command, so the daemon's own process directory is never observable as
session state. Its name is also optional; an omitted name receives a
daemon-assigned unique name. The exact automatic naming policy is an
implementation detail rather than a version-7 wire guarantee.

`kill_session` is a one-response command that explicitly terminates the
selected session and therefore ends every attachment. It is distinct from
`detach`, transport EOF, and client exit, all of which preserve the session.
Only explicit `detach` releases leases immediately; unexpected EOF retains
them until resume or grace expiry.

`attach_session` includes the attaching terminal size and requests for input
and layout leases, plus independent `request_command_line` and
`request_running_command` privacy requests.
Requesting an unheld layout lease is an explicit resize: the daemon applies
that size to the shared canvas before sending `attached`. Without that request, an attach
never resizes the view; the size only lets the daemon report when a checkpoint
was made for another layout. Requesting command-line state never grants access
by itself; daemon policy may redact it.

The terminal size in `attached` is an authoritative PTY-layout fact, not an
instruction to resize a client viewport. Later layout changes arrive as
replacing `checkpoint` stream messages so every attached renderer can adopt
the authoritative reflow without gaining layout ownership.

`get_shell_state` is a one-response command for a noninteractive current-state
lookup. It returns `shell_state_response` with the resolved `session` and the
same complete `shell_state` model used by an attachment. Editable command-line
and running-command visibility remain subject to daemon policy.

## Attachment liveness

`handshake_accepted` advertises a heartbeat interval and an attachment-liveness
timeout. A standard interactive client sends `heartbeat { nonce }` at the
advertised cadence; `ctmuxd` replies with `heartbeat_ack { nonce }`. Any valid
post-attach client message also demonstrates client liveness.

The client's peer-silence deadline is independent of outbound writes. A blocked
input or heartbeat write cannot postpone detecting a silent daemon. Incoming
activity renews that deadline even while an outgoing write remains blocked.

If no client activity reaches `ctmuxd` before the timeout, it closes that
transport generation. The logical attachment remains resumable only for its
bounded grace; expiry releases its leases. It does not kill the PTY, shell,
journal, or checkpoint state. This makes a laptop sleep, client crash, or
half-open network path unable to pin input or layout ownership forever.

The initial `attached` reply and later presentation use a separate five-minute
delivery deadline. `ctmuxd` sends only a bounded presentation window beyond the
last `presentation_applied` sequence, so a slow renderer applies backpressure
without turning queue capacity into a transport failure. Heartbeats and control
messages continue while presentation is paused. The delivery deadline still
keeps a client that stops reading entirely from retaining leases indefinitely.

The deadline has priority over a late client frame: an expired attachment
cannot revive itself with a late heartbeat, input, resize, or lease request.
Clients also treat a peer that remains silent for the advertised timeout as a
lost connection and reconnect by session ID, attachment token, and renderer-
applied output sequence.

## Presentation flow control

`attach_session` and `resume_attachment` advertise a non-zero
`presentation_window_bytes`. The daemon charges raw output against that window,
with a minimum charge per frame so fragmented PTY reads cannot create an
unbounded event count. A checkpoint blocks later output until the renderer has
applied it.

After a renderer completes a checkpoint or output event, the client sends
`presentation_applied { sequence }`. This is delivery credit, not a state hash:
it proves only that the presentation event finished. A client that could not
adopt a checkpoint or geometry may replenish delivery credit while keeping its
own reconnect cursor unset. Heartbeats, detach, input, and lease control remain
independent of presentation credit.

When the child exits, the daemon continues accepting presentation acknowledgements
and pinned history page requests until all final output and requested snapshot
bytes have been sent. It then sends the final shell state and
`session_ended`; the last output frame need not be acknowledged before closure.
This drain keeps the attachment's existing liveness deadline fixed, so a renderer
that stops applying output cannot retain an ended session indefinitely by sending
heartbeats. A stalled attachment closes when that deadline expires.

## Attachment leases

Every `attach_session` creates one logical attachment. Input and layout are
separate attachment-bound leases:

- `request_input_lease` and `request_layout_lease` claim each unheld lease as
  part of the attach operation. They never displace another attachment.
- `acquire_lease` and `release_lease` adjust one capability after attaching.
  The daemon replies with `lease_status`, whose `owned_by_client` field is
  relative to that attachment; other attachment identities are not exposed.
- `attached` contains the initial input and layout lease statuses.
- `input` requires the input lease, and `resize` requires the layout lease.
  An unauthorized command receives a structured error but does not terminate
  the shell session.
- A successful resize that changes the PTY's geometry sends a replacing
  `checkpoint` and new history manifest to every live attachment. It changes neither input
  ownership nor a client's viewport, scroll position, or selection.
- Explicit detach and reconnect-grace expiry release any leases owned by that
  attachment. Transport EOF, write failure, and attachment-liveness expiry
  first preserve them for bounded reconnect. None terminate the PTY or child
  process.

`attached.attachment_token` is random, session-scoped, memory-only, and never
exposes the daemon-private attachment ID. `resume_attachment` with a valid
token immediately supersedes the previous transport generation, including a
half-open one, and returns the same token and current lease status. An invalid
or expired token receives `attachment_resume_rejected`. Output recovery remains
independent: the client must still supply only its renderer-applied
`resume_from`, and input is never replayed.

The two requested leases are intentionally independent. A desktop client can
retain input while a separate client owns layout, and a viewer can attach
without requesting either capability.

## Shell awareness

Shell-awareness metadata is optional, advisory session state beside the raw VT
journal. It never replaces raw output, terminal checkpoints, or the client's
own viewport and selection state. The daemon must not infer a directory,
command line, shell, or prompt from rendered terminal text.

An attached client always receives a complete `shell_state` in `attached`. It
starts as an explicit revision-zero unknown snapshot; OS observations can fill
cwd and process details even without shell integration. Each later
`shell_state_changed` is a complete replacement snapshot, not a patch. Its session-scoped `revision` increases
strictly; clients ignore an update that is not newer than their current
revision **within the same attachment**. The initial snapshot of a new
attachment is authoritative even when its revision matches a locally cached
snapshot, because input-lease visibility can make its command metadata more
restricted. When an attachment that requested editable command-line or
running-command metadata newly gains the input lease while the corresponding
value exists, `ctmuxd` emits an otherwise unchanged newer snapshot. This lets a
previously redacted client converge without weakening the monotonic revision
rule.

`observed_sequence` is the raw-output **next offset** when the daemon observed
the state: all raw bytes below the offset have reached the daemon. It is useful
only for correlation and display ordering; it is never a resume cursor. A
client may defer presenting a state change until it has rendered raw output
through that offset.

The state contains:

- `shell`: a descriptor with `shell_type` (`bash`, `zsh`, `fish`, `pwsh`,
  `cmd`, `sh`, or `unknown`), an optional integration-format version, and
  advertised reporting capabilities. A trusted shell integration can report a
  new descriptor; the shipped integrations intentionally do not pass their
  private reporter capability to arbitrary command descendants.
- `cwd`: the unmodified, shell-reported working directory, falling back to the
  root shell's OS-observed physical cwd when no report supplies it. A client may
  send it back to the same daemon for an operation such as creating a new session in
  the current directory, but it is not portable across hosts and grants no
  filesystem authority.
- `cwd_display`: an optional daemon-derived presentation of `cwd`. The daemon
  replaces its own user-home prefix with `~`; clients fall back to `cwd` when
  reading a snapshot from an older daemon. Clients must not use this display
  value as an operational filesystem path.
- `cwd_source`: optional `shell_integration` or `process`, absent when cwd is
  unavailable or a snapshot comes from an older daemon.
- `process`: optional native observation containing the root `pid`, optional
  bounded `name`, and `foreground`. The latter is tagged with `state`:
  `unknown`, `shell` (the root shell's group owns the tty), or `child` with a
  verified descendant's `pid` and optional `name`. Names are nonempty, at most
  256 UTF-8 bytes, with no control characters; no arguments or environment are
  collected. This is independent of integration capabilities and command-text
  visibility. Shell-group ownership does not establish prompt state; builtins
  and jobs running without job control cannot reliably be distinguished.
- `prompt_phase`: `unknown`, `at_prompt`, `editing`, or `running`.
- `current_command_line`: an optional editable buffer with an optional cursor
  measured in Unicode scalar values, not terminal columns or UTF-8 bytes.
- `running_command`: an optional non-editable title summary while
  `prompt_phase` is `running`. It is nonempty, at most 256 UTF-8 bytes, and
  contains no control characters. It is not parsed as a process identity or
  command invocation.
- `tui_hint`: `unknown`, `inline`, or `alternate_screen`. The final value is a
  terminal-parser observation, not a claim that an application is or is not a
  TUI. Some TUIs do not use the alternate screen, and some ordinary programs
  do.

`cwd_source` and `process` are additive, default-absent protocol-9 fields.
Older clients ignore them, and newer clients accept older snapshots. Native
observations continue without attachments. OS failures produce absent/unknown
details rather than failing the session; process exit clears live identity.
Physical cwd can differ from logical `$PWD`, and names can be truncated by the
OS. Snapshots are best-effort observations, not guarantees of liveness.

Shell integration reports use a daemon-private, per-session reporter sink and
cannot be submitted through a normal client protocol command. The current Unix
implementation uses a unique mode-`0600` FIFO supplied to a session child as
`CTMUX_SHELL_STATE_PIPE`; future platforms can provide an equivalent private
sink. Shipped shell integrations copy the pathname into a non-exported shell
variable, remove `CTMUX_SHELL_STATE_PIPE`, and open/write/close the FIFO for
each report. Commands executed by that shell therefore inherit neither the
environment variable nor a reporter file descriptor. Reporter records never
pass through the raw PTY journal, so their separate command-buffer copy cannot
enter terminal replay or future journal persistence. Reports remain untrusted
advisory input: shell-awareness state must never authorize operations or
control lease ownership. The daemon assigns both `revision` and
`observed_sequence`.

The live command buffer and short running-command summary can contain secrets.
They are deliberately absent from `session_info` and `list_sessions`. An
attachment must explicitly request each value and currently own the input
lease; the daemon may return `command_line_redacted: true` with
`current_command_line: null` and/or `running_command_redacted: true` with
`running_command: null` under its visibility policy. `get_shell_state`
redacts both because a one-shot query has no input-lease identity. The shipped
integrations clear editable text when a command starts, and ctmuxd clears both
active-text forms when a session ends. This is metadata-channel redaction, not
a guarantee that typed characters are secret from ordinary terminal viewers:
shell line editing often echoes them into the canonical raw PTY output journal.
Daemon shell-awareness state is memory-only. The desktop workspace explicitly
retains only last-known cwd/display-cwd for presentation; it does not persist
editable text, running commands, or live prompt state. See `ctmux-workspace.md`.

FIFO report version 2 retains the version-1 record shape: exactly nine
NUL-delimited fields. The first field is `ctmux-shell-v2`; fields seven through
nine are phase-exclusive active text. During `at_prompt` or `editing`, they
mean `command_line_present`, `command_line`, and `cursor_scalar_offset` just
as in version 1. During `running`, they mean `running_command_present`,
`running_command`, and an empty cursor field. `unknown` reports no active
text. `ctmuxd` continues to accept `ctmux-shell-v1` records with their original
editable-command semantics, so installed v1 integrations remain compatible.

An attachment recovering from bounded output-broadcast lag may miss state
updates. After sending a recovery checkpoint, the daemon sends its latest
complete shell-state snapshot again. This lets clients converge without a
separate shell-state replay cursor.

## Stream sequences

Sequences are byte offsets in the raw PTY output stream. An output frame owns
the half-open range `[sequence_start, sequence_end)`, where
`sequence_end = sequence_start + data.len()`.

The first output byte has sequence zero. Sequence values never move backwards
or reset during a session.

An attaching client may provide `resume_from`:

- omitted: restore the latest compatible checkpoint and replay raw output
  after it;
- within the retained range: replay from that byte;
- older than retained history: replay from the earliest retained byte and set
  `history_gap`;
- greater than the next sequence: reject the attach request.

## PTY geometry transitions

Every changed PTY geometry creates an authoritative checkpoint and paired new
history manifest. This replaces the client grid rather than asking a renderer
with incomplete history to reconstruct remote reflow. A resize does not grant
input ownership or change another client's viewport.

The daemon orders replacement after earlier raw output has been applied and
before later output is sent. Several resizes can share a raw byte sequence;
the daemon's internal replacement revision distinguishes these boundaries.
Each replacement has a fresh `snapshot_id`, invalidating previous history jobs.
The `pty_geometry_changed` variant remains in the protocol for presentation
adapters, but internal build 14 daemon geometry delivery uses checkpoints.

AVT defers reflow of a hidden primary buffer while alternate screen is active.
If geometry changes there, returning to primary also replaces the checkpoint
and history manifest, after the primary buffer adopts the current geometry.

A resume at or before the latest geometry or history-clear boundary receives a
checkpoint. When its boundary remains replayable, the daemon uses that exact
checkpoint; otherwise it replaces from a newer one and reports `history_gap`.
A renderer that cannot adopt the authoritative checkpoint must leave its
reconnect cursor unset.

## Checkpoints

When a client has no previous sequence, its requested sequence is older than
retained raw output, or the request crosses a PTY geometry boundary, `attached`
includes a terminal checkpoint and a terminal-history snapshot with the same
`sequence`. A fresh attachment captures the current screen rather than waiting
for the periodic journal checkpoint. The client restores the live checkpoint
and small recent history tail, then processes output from `replay_from` forward
while fetching full history pages. A checkpoint and history snapshot are
invalid unless both are present and their sequences match.

`history_gap` means the restored presentation is not complete back to the
client's requested position. It can be caused by bounded journal eviction, a
geometry-safe checkpoint fallback, or eviction of the oldest normalized
history lines. The live terminal state remains complete when a compatible
checkpoint was supplied, but clients must expose the history discontinuity
rather than invent missing lines.

The version-1 checkpoint format is:

```text
format:         ctmux_vt_state
format_version: 1
sequence:       raw stream position represented by the checkpoint
terminal_size:  PTY dimensions used to generate it
payload:        VT restore stream for terminal and parser state
input_prefix:   raw bytes that must follow payload before later output
```

`input_prefix` exists so an incomplete UTF-8 sequence at a checkpoint boundary
is completed by later raw output without changing the checkpoint's parser
state. It is part of the checkpoint format, not an additional output record.

The version-1 terminal-history format is:

```text
format:          ctmux_logical_lines
format_version:  1
sequence:        raw boundary shared with the checkpoint
generation:      identity reset by RIS or erase-saved-lines
revision:        monotonic snapshot revision within daemon memory
retained_bytes:  normalized UTF-8 bytes retained, including line separators
truncated:       whether older lines in this generation were evicted
lines:           recent completed logical lines at manifest.first_line
```

History lines are the daemon emulator's normalized text after terminal controls
have been interpreted. Soft-wrapped physical rows are merged into logical
lines. Alternate-screen output is excluded. The snapshot is bounded by bytes,
physical rows, and emulator cells; its completed transfer is a full replacement,
not an incremental patch. Version 1 does not preserve style runs in historical lines.

The emulator strips trailing whitespace from each complete logical line before
history storage; hard line breaks do not pad stored lines to the terminal width.
This also strips explicitly written trailing whitespace, which the normalized
text does not distinguish from unused cells. Leading and interior spaces remain,
including the column offset preserved by a bare LF (without CR). Soft-wrapped
rows are joined before trailing whitespace is stripped.

`terminal_size` in a checkpoint is authoritative for its restored parser
state. A graphical client must reset or recreate its terminal model at those
dimensions before applying `payload` and `input_prefix`; it must not apply a
checkpoint restore stream into an unrelated live grid. The checkpoint
supersedes every geometry transition represented when that checkpoint was
captured; a later live resize can legitimately share its raw sequence boundary
when no output was produced between the two operations.

If a live attachment falls behind its output broadcast buffer, `checkpoint`
is sent with its paired history as a stream message and clients restore both
before accepting later output. A recovery checkpoint also provides the current PTY geometry, so it
supersedes any queued geometry transition it already covers. A client must
reject a checkpoint or history format/version it does not support.

## Paged history projection (published contract 1.1.14)

Contract `1.0.13` remains supported with complete inline history and no paged
manifest. Paged messages and lease-free checkpoint recovery are sent only when
`1.1.14` or later is selected. Paged history was introduced in internal build 14.

`attached` and `checkpoint` include `history_manifest` whenever they carry a
checkpoint/history pair. Delta-only resumes omit all three. The manifest is:

```text
snapshot_id:     opaque ID of this attachment's frozen transfer snapshot
sequence:        raw boundary shared with checkpoint and recent history
generation:      saved-history clear/reset epoch
revision:        rolling history window revision
scrollback_limit: physical rows retained by the paired terminal emulator
total_rows:      number of physical primary-scrollback rows
total_bytes:     byte length of canonical JSONL
total_lines:     completed normalized logical lines, after the 4 MiB cap
first_line:      start index of the inline recent logical tail
truncated:       earlier source history was discarded
content_hash:    lowercase SHA-256 hex of all canonical JSONL bytes
```

Each canonical row is a compact JSON object with `text` and `wrapped`, followed
by a newline. Text preserves physical row padding and wrapping. Rows above the
live grid include a partial wrapped prefix which still continues on screen;
normalization excludes that prefix from completed logical history. Alternate
screen checkpoints retain the primary rows captured before switching buffers.
When alternate-screen resize reduces the physical row budget, only the newest
captured primary rows remain and `truncated` becomes true. Historical styling
is not retained.

The inline recent tail is bounded by 64 logical lines and 16 KiB of encoded
line content. It does not imply that older history is missing. Clients display
the screen immediately and acknowledge it independently of history transfer.

`history_request { snapshot_id, offset, max_bytes }` is valid only on an
attachment stream and requires no lease. `offset` is a JSONL byte offset;
`max_bytes` must be nonzero and is capped at 16 KiB. The reply is
`history_page { snapshot_id, offset, data, next_offset }`. Chunks may split JSON
rows or UTF-8 characters; assemble them before decoding. `next_offset: null`
marks the final chunk. Validate byte count, row count, and content hash before
replacing the local projection. Snapshot-relative offsets are not permanent
line identities, and hashes verify content rather than infer overlap.

One snapshot, at most 16 MiB encoded, is pinned per attachment. Successful page
access renews its two-minute idle expiry. A new checkpoint replaces the pin and
cancels any queued request for its previous manifest; clients cancel that
previous job when adopting the new checkpoint. Requests for expired or replaced
IDs return `history_snapshot_expired { snapshot_id }`; ignore expiration for a
job already superseded by another manifest. Invalid offsets or
zero page sizes return `invalid_request`. At most one page request is pending.
Small pages follow bounded output/control turns so neither screen delivery nor
background history starves the other.

`request_checkpoint` queues a fresh authoritative screen/history replacement
without acquiring a lease. It waits for pending presentation credit and
in-flight output acknowledgements while input and heartbeats remain responsive.
Use it after expiry or loss of a coherent local replay baseline.

Normal child exit retains its pinned history transfer until complete within
the existing fixed drain deadline, then delivers `session_ended`. Explicit
detach, transport loss, or deadline expiry can interrupt backfill; clients must
show the incomplete-history state rather than treat the recent tail as complete.

## Deliberately deferred

- disk-backed journals;
- disk-backed shell-awareness metadata;
- durable command-line visibility and authorization policy;
- process restart policies and generations;
- Windows named-pipe transport.

## Shared pane zoom (published contract 1.1.15)

Internal build 15 adds the attached `set_view_zoom { terminal_id }` operation.
A terminal ID zooms that member of the attachment's view; `null` restores the
saved split layout. The attachment must own that view's layout lease. Input
ownership is independent, and zoom never takes another client's lease. Invalid
targets and denied ownership produce nonfatal control errors.

`view_snapshot` adds optional `zoomed_terminal_id`. The saved `layout` and
`panes` retain every terminal and its ordinary split rectangle; `terminals`
reports each actual PTY size. When zoomed, clients render only that terminal at
`(0, 0)` with the full `canvas_size`. Membership always comes from `terminals`,
so hiding a pane does not close its attachment or discard copy state.

Zoom enlarges the selected PTY; hidden PTYs retain their dimensions until
unzoom. A canvas resize while zoomed resizes the selected PTY. Unzoom reflows
the saved splits into the current canvas. Layout/membership mutations clear
zoom before reflow, and exit of the zoomed terminal clears zoom. Detach and
reconnect retain shared zoom. Geometry checkpoints continue to replace live
screen/history boundaries; frozen local copy selections remain independent.

The mutation is acknowledged with a `view_snapshot` on its attachment stream.
New-contract attached clients also receive coalesced view snapshots after shared
geometry/zoom changes. Clients use view revisions to discard stale updates.

Contracts `1.0.13` and `1.1.14` remain implemented. Their snapshots omit the zoom
field and their attachment streams do not receive unsolicited view snapshots.
They retain the ordinary split grid and all terminal membership; while a newer
owner has zoomed a PTY, older viewers clip that PTY's output to its split region.
They cannot request zoom. New clients disable zoom after negotiating an older
contract.

## Shared pane sizing (published contract 1.1.16)

Internal build 16 adds split `weights` and the attached request
`resize_pane { request_id, terminal_id, direction, amount }`. Weights are relative
positive integers; an omitted or empty list retains the historical equal split.
A nonempty list must match the number of children. For example:

```json
{
  "kind": "split",
  "axis": "horizontal",
  "weights": [30, 69],
  "children": [
    { "kind": "terminal", "terminal_id": "terminal-a" },
    { "kind": "terminal", "terminal_id": "terminal-b" }
  ]
}
```

Allocation reserves one cell per divider, clamps children at their recursive
minimum sizes, and divides the remaining cells proportionally. Integer residual
cells go to earlier children deterministically. Pane rectangles are authoritative
for every client; clients do not allocate from weights themselves. Canvas resizing
retains the saved ratios, subject to minimum sizes and integer rounding.

`request_id` is a client-generated opaque string of 1–256 UTF-8 bytes.
`direction` is `left`, `right`, `up`, or `down`; `amount` is a positive `u16` cell
count. The attachment must own the view-wide layout lease, and `terminal_id` must
identify a live member of that view. The nearest ancestor split with the requested
axis supplies the divider after the selected child, or the preceding divider if
that child is last. Left/up moves that divider negatively; right/down moves it
positively. The two adjacent subtree minima limit movement. Other siblings keep
their current extents. A layout with no matching divider, or a divider already at
its limit, produces an unchanged successful result.

A changed resize atomically clears zoom, reflows all member PTYs, and increments
the view revision. Invalid requests and unchanged movements retain layout and
zoom. Reflow failure restores the previous layout and zoom. Proportions survive
split, terminal removal, session merge, and reconnect within the daemon lifetime;
new splits begin equal and redundant one-child nodes still collapse.

The reply is `pane_resize_result { request_id, outcome }`, where `outcome` is
`{ "kind": "applied", "view": ... }` or
`{ "kind": "rejected", "code": ..., "message": ... }`. Applied includes the
current snapshot for an unchanged movement. Clients correlate the request ID
instead of treating unrelated view broadcasts as acknowledgements. Rejection is
nonfatal. Shared view broadcasts continue to notify other attached viewers.

Contracts `1.0.13`, `1.1.14`, and `1.1.15` remain supported. Their view snapshots
omit weights while retaining authoritative unequal pane rectangles. Contract
`1.1.15` retains zoom and view broadcasts. Earlier contracts cannot request pane
resizing or supply weights. New clients disable resizing after negotiating an
earlier contract.

One-shot `update_view` remains an arrangement operation, not a resize path. Updates
with matching split axes and child counts preserve the existing positional weights,
including updates from clients that cannot send them. Changing explicit weights or
restructuring a weighted node ambiguously is rejected; clients must use the attached
resize operation to change proportions. This prevents arrangement updates from
bypassing resize ownership or silently restoring equal sizes.

The divider directions and default Ctrl-arrow/Alt-arrow increments follow tmux's
[resize command](https://github.com/tmux/tmux/blob/master/cmd-resize-pane.c) and
[key bindings](https://github.com/tmux/tmux/blob/master/key-bindings.c).
