# rmux protocol version 9

The protocol is independent of local IPC and future remote transport. Version
9 uses length-prefixed JSON frames for debuggability. Each frame begins with a
four-byte unsigned big-endian payload length.

The maximum encoded frame size is 8 MiB.

### Version 7 attachment reconnect

Version 7 adds an opaque `attachment_token` to `attached` and a corresponding
`resume_attachment` request. The token rebinds a replacement transport to the
same logical attachment during a bounded reconnect grace, preserving its input
and layout leases. Protocol versions still match exactly during handshake.

Version 8 adds renderer-applied presentation flow control. Version 9 pairs
every terminal checkpoint with a bounded normalized-history snapshot captured
at the same raw sequence.

## Connection lifecycle

Every connection starts with `handshake`. The daemon replies with
`handshake_accepted` or a structured error. One command follows a successful
handshake.

Most commands receive one response and close. `attach_session` creates a
logical attachment and changes the connection into a bidirectional stream.
`resume_attachment` rebinds a replacement stream to an existing logical
attachment:

- daemon to client: `attached` (including a complete `shell_state` snapshot),
  an optional checkpoint/history pair, replayed `output`, then live `output`,
  `pty_geometry_changed`, and `shell_state_changed`;
- client to daemon: `input`, `resize`, lease acquire/release, `heartbeat`,
  `presentation_applied`, or `detach`;
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
that terminal size before sending `attached`. Without that request, an attach
never resizes the PTY; the size only lets the daemon report when a checkpoint
was made for another layout. Requesting command-line state never grants access
by itself; daemon policy may redact it.

The terminal size in `attached` is an authoritative PTY-layout fact, not an
instruction to resize a client viewport. Later layout changes arrive as
`pty_geometry_changed` stream messages so every attached renderer can update
its grid without gaining layout ownership.

`get_shell_state` is a one-response command for a noninteractive current-state
lookup. It returns `shell_state_response` with the resolved `session` and the
same complete `shell_state` model used by an attachment. Editable command-line
and running-command visibility remain subject to daemon policy.

## Attachment liveness

`handshake_accepted` advertises a heartbeat interval and an attachment-liveness
timeout. A standard interactive client sends `heartbeat { nonce }` at the
advertised cadence; `rmuxd` replies with `heartbeat_ack { nonce }`. Any valid
post-attach client message also demonstrates client liveness.

If no client activity reaches `rmuxd` before the timeout, it closes that
transport generation. The logical attachment remains resumable only for its
bounded grace; expiry releases its leases. It does not kill the PTY, shell,
journal, or checkpoint state. This makes a laptop sleep, client crash, or
half-open network path unable to pin input or layout ownership forever.

The initial `attached` reply and later presentation use a separate five-minute
delivery deadline. `rmuxd` sends only a bounded presentation window beyond the
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
- A successful resize that changes the PTY's geometry sends
  `pty_geometry_changed` to every live attachment. It changes neither input
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

An attached client always receives a complete `shell_state` in `attached`. An
unintegrated session uses the explicit revision-zero unknown snapshot rather
than omitting the field. Each later `shell_state_changed` is a complete
replacement snapshot, not a patch. Its session-scoped `revision` increases
strictly; clients ignore an update that is not newer than their current
revision **within the same attachment**. The initial snapshot of a new
attachment is authoritative even when its revision matches a locally cached
snapshot, because input-lease visibility can make its command metadata more
restricted. When an attachment that requested editable command-line or
running-command metadata newly gains the input lease while the corresponding
value exists, `rmuxd` emits an otherwise unchanged newer snapshot. This lets a
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
- `cwd`: the unmodified, shell-reported working directory. A client may send it
  back to the same daemon for an operation such as creating a new session in
  the current directory, but it is not portable across hosts and grants no
  filesystem authority.
- `cwd_display`: an optional daemon-derived presentation of `cwd`. The daemon
  replaces its own user-home prefix with `~`; clients fall back to `cwd` when
  reading a snapshot from an older daemon. Clients must not use this display
  value as an operational filesystem path.
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

Shell integration reports use a daemon-private, per-session reporter sink and
cannot be submitted through a normal client protocol command. The current Unix
implementation uses a unique mode-`0600` FIFO supplied to a session child as
`RMUX_SHELL_STATE_PIPE`; future platforms can provide an equivalent private
sink. Shipped shell integrations copy the pathname into a non-exported shell
variable, remove `RMUX_SHELL_STATE_PIPE`, and open/write/close the FIFO for
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
integrations clear editable text when a command starts, and rmuxd clears both
active-text forms when a session ends. This is metadata-channel redaction, not
a guarantee that typed characters are secret from ordinary terminal viewers:
shell line editing often echoes them into the canonical raw PTY output journal.
The metadata is memory-only unless a future explicit persistence policy says
otherwise.

FIFO report version 2 retains the version-1 record shape: exactly nine
NUL-delimited fields. The first field is `rmux-shell-v2`; fields seven through
nine are phase-exclusive active text. During `at_prompt` or `editing`, they
mean `command_line_present`, `command_line`, and `cursor_scalar_offset` just
as in version 1. During `running`, they mean `running_command_present`,
`running_command`, and an empty cursor field. `unknown` reports no active
text. `rmuxd` continues to accept `rmux-shell-v1` records with their original
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

`pty_geometry_changed` is an authoritative, absolute PTY-layout update:

```text
terminal_size:     columns, rows, and optional pixel dimensions
observed_sequence: raw-output next offset at the transition
```

For a transition at `observed_sequence = S`, the daemon sends every raw output
byte below `S`, then the geometry message, then any raw output byte at or above
`S`. If several resize operations occur before another output byte, their
messages share `S` and remain in daemon stream order. A renderer applies the
absolute size in each message; repeating a same-size message is harmless.

The message reports a PTY fact only. It never grants the layout lease, requests
that another attachment resize, or controls a client's viewport. In particular,
a narrow phone viewer receives the desktop session's geometry but does not
change it.

A presenter that cannot render at the announced grid must not advance its
local reconnect cursor past the transition. It can continue to show best-effort
output, but its next attach must omit `resume_from` so `rmuxd` supplies a
geometry-safe checkpoint.

Geometry transitions are not raw VT bytes and the protocol deliberately does not
add a second resume cursor for them. A caller may use `resume_from` only for a
renderer that has processed the complete prior attachment stream, including
geometry messages. If the requested raw position is at or before the most
recent geometry boundary, `rmuxd` sends a checkpoint instead of replaying raw
output across that change. When the resize-boundary checkpoint remains usable,
a request exactly at the boundary replays from that same offset without a
history gap. If a checkpoint must advance past the requested raw position,
`history_gap` is true even when those raw bytes are still retained; this makes
the client mark its own local history as discontinuous rather than reconstruct
a renderer with the wrong grid.

## Checkpoints

When a client has no previous sequence, its requested sequence is older than
retained raw output, or the request crosses a PTY geometry boundary, `attached`
includes a terminal checkpoint and a terminal-history snapshot with the same
`sequence`. The client restores the history and live checkpoint, then processes
output from `replay_from` forward. A checkpoint and history snapshot are invalid
unless both are present and their sequences match.

`history_gap` means the restored presentation is not complete back to the
client's requested position. It can be caused by bounded journal eviction, a
geometry-safe checkpoint fallback, or eviction of the oldest normalized
history lines. The live terminal state remains complete when a compatible
checkpoint was supplied, but clients must expose the history discontinuity
rather than invent missing lines.

The version-1 checkpoint format is:

```text
format:         rmux_vt_state
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
format:          rmux_logical_lines
format_version:  1
sequence:        raw boundary shared with the checkpoint
generation:      identity reset by RIS or erase-saved-lines
revision:        monotonic snapshot revision within daemon memory
retained_bytes:  normalized UTF-8 bytes retained, including line separators
truncated:       whether older lines in this generation were evicted
lines:           complete logical lines above the live grid
```

History lines are the daemon emulator's normalized text after terminal controls
have been interpreted. Soft-wrapped physical rows are merged into logical
lines. Alternate-screen output is excluded. The snapshot is bounded by bytes,
physical rows, and emulator cells; it is a full replacement, not an incremental
patch. Version 1 does not preserve style runs in historical lines.

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

## Deliberately deferred

- disk-backed journals;
- disk-backed shell-awareness metadata;
- durable command-line visibility and authorization policy;
- process restart policies and generations;
- Windows named-pipe transport.
