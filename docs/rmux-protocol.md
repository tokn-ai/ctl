# rmux protocol version 2

The protocol is independent of local IPC and future remote transport. Version
1 uses length-prefixed JSON frames for debuggability. Each frame begins with a
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
- client to daemon: `input`, `resize`, or `detach`;
- daemon to client: `session_ended` when the child exits.

Transport disconnection is an implicit detach and never kills the session.
Creating a session carries its initial working directory separately from the
optional command, so the daemon's own process directory is never observable as
session state.

`attach_session` includes the attaching terminal size. This does not resize
the PTY; it lets the daemon report when a checkpoint was made for a different
layout.

## Stream sequences

Sequences are byte offsets in the raw PTY output stream. An output frame owns
the half-open range `[sequence_start, sequence_end)`, where
`sequence_end = sequence_start + data.len()`.

The first output byte has sequence zero. Sequence values never move backwards
or reset during a session.

An attaching client may provide `resume_from`:

- omitted: replay all retained history;
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
- multiple-client input and layout leases;
- Windows named-pipe transport.
