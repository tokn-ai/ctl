# rmux protocol version 4

The protocol is independent of local IPC and future remote transport. Version
4 uses length-prefixed JSON frames for debuggability. Each frame begins with a
four-byte unsigned big-endian payload length.

The maximum encoded frame size is 8 MiB.

## Connection lifecycle

Every connection starts with `handshake`. The daemon replies with
`handshake_accepted` or a structured error. One command follows a successful
handshake.

Most commands receive one response and close. `attach_session` changes the
connection into a bidirectional stream:

- daemon to client: `attached`, an optional checkpoint, replayed `output`,
  then live `output`;
- client to daemon: `input`, `resize`, lease acquire/release, `heartbeat`, or
  `detach`;
- daemon to client: `heartbeat_ack` and `session_ended` when the child exits.

Transport disconnection is an implicit detach and never kills the session.
Creating a session carries its initial working directory separately from the
optional command, so the daemon's own process directory is never observable as
session state.

`attach_session` includes the attaching terminal size and requests for input
and layout leases. Requesting an unheld layout lease is an explicit resize: the
daemon applies that terminal size before sending `attached`. Without that
request, an attach never resizes the PTY; the size only lets the daemon report
when a checkpoint was made for another layout.

## Attachment liveness

`handshake_accepted` advertises a heartbeat interval and an attachment-liveness
timeout. A standard interactive client sends `heartbeat { nonce }` at the
advertised cadence; `rmuxd` replies with `heartbeat_ack { nonce }`. Any valid
post-attach client message also demonstrates client liveness.

If no client activity reaches `rmuxd` before the timeout, it closes that
attachment and releases its connection-bound leases. It does not kill the PTY,
shell, journal, or checkpoint state. This makes a laptop sleep, client crash,
or half-open network path unable to pin input or layout ownership forever.

The initial `attached` reply, checkpoint, and retained-output replay use a
separate five-minute delivery deadline. The liveness deadline starts only
after that transfer finishes, because a client cannot begin its advertised
heartbeat loop until it has received `attached`, and `rmuxd` serially sends
the replay before it can process queued heartbeats. The delivery deadline still
keeps a client that stops reading during initial replay from retaining leases
indefinitely.

The deadline has priority over a late client frame: an expired attachment
cannot revive itself with a late heartbeat, input, resize, or lease request.
Clients also treat a peer that remains silent for the advertised timeout as a
lost connection and reconnect by session ID and output sequence.

## Attachment leases

Every `attach_session` stream is one attachment. Input and layout are separate
connection-bound leases:

- `request_input_lease` and `request_layout_lease` claim each unheld lease as
  part of the attach operation. They never displace another attachment.
- `acquire_lease` and `release_lease` adjust one capability after attaching.
  The daemon replies with `lease_status`, whose `owned_by_client` field is
  relative to that attachment; other attachment identities are not exposed.
- `attached` contains the initial input and layout lease statuses.
- `input` requires the input lease, and `resize` requires the layout lease.
  An unauthorized command receives a structured error but does not terminate
  the shell session.
- Detach, transport EOF, write failure, attachment-liveness expiry, and session
  end release any leases owned by that attachment. They do not terminate the
  PTY or child process.

The two requested leases are intentionally independent. A desktop client can
retain input while a separate client owns layout, and a viewer can attach
without requesting either capability.

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

## Checkpoints

When a client has no previous sequence, or its requested sequence is older
than retained raw output, `attached` includes a terminal checkpoint. The client
restores it and then processes output from `replay_from` forward. `history_gap`
indicates that older scrollback is unavailable; it does not mean the visible
terminal state is incomplete when a compatible checkpoint was supplied.

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

If a live attachment falls behind its output broadcast buffer, `checkpoint`
is sent as a stream message and clients restore it before accepting later
output. A client must reject a checkpoint whose `format` or `format_version` it
does not support.

## Deliberately deferred

- disk-backed journals;
- cwd shell integration;
- process restart policies and generations;
- Windows named-pipe transport.
