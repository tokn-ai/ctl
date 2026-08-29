# Architecture

The monorepo contains two products with independent responsibilities:

- `rmux` provides persistent local terminal sessions.
- `ctl` provides authenticated remote device control and will expose `rmux`
  sessions without reimplementing terminal persistence.

Only `rmux` is part of the current milestone.

## Process ownership

`rmuxd` is a per-user daemon. It owns every PTY and child process. A local
`rmux` client connects over per-user IPC and may disappear without affecting a
session.

In the future, `ctld` will act as an authenticated gateway:

```text
local:  rmux -> local IPC -> rmuxd -> PTY -> shell
remote: ctl  -> network   -> ctld  -> local IPC -> rmuxd -> PTY -> shell
```

`ctld` and `rmuxd` have independent lifecycles. Restarting `ctld` must not
affect a terminal session. If `rmuxd` itself exits, an exact running PTY is not
recoverable in the initial architecture. Later disk-backed metadata may
reconstruct explicitly restartable tasks as a new process generation.

## Crate boundaries

- `rmux-proto`: versioned, platform-independent wire messages and framing.
- `rmux-core`: output journal and portable session-domain behavior.
- `rmux-client`: portable client-side protocol state, checkpoint restoration,
  and terminal attachment behavior over an injected byte stream.
- `rmux-ipc`: per-user local endpoint selection and transport setup.
- `rmuxd`: local IPC, PTY/process ownership, and session coordination.
- `rmux`: local command-line adapter that starts/connects to `rmuxd` over IPC.

OS-specific IPC and PTY implementation details must not enter `rmux-proto` or
`rmux-client`.
The initial IPC implementation targets Unix-domain sockets on macOS and Linux;
Windows named pipes will be a separate transport implementation.

## Current invariants

1. The daemon, never the client, owns the PTY and child process.
2. Disconnecting a client does not terminate a session.
3. Output is persisted as raw bytes in a bounded in-memory journal.
4. Output positions are session-global, monotonically increasing byte offsets.
5. Reattachment resumes from an explicit stream sequence.
6. The daemon starts on demand and exits after its final session and client are
   gone.
7. Checkpoints are derived from raw VT output and never replace it as the
   canonical session record.
8. A reconnect after journal compaction restores a checkpoint and replays only
   later raw output.
9. Each live attachment may view a session independently, but input and PTY
   layout are independently leased capabilities.
10. An attachment can claim only an unheld lease; it never implicitly takes a
    lease from another attachment.
11. Leases are connection-bound. They are released when their attachment
    detaches or disconnects, while the shell session itself continues.

An ordinary attaching client requests input only when no other attachment owns
it. It does not resize an existing PTY. A client must explicitly request the
layout lease before its terminal size is applied, so a small secondary client
cannot disturb an established desktop layout.

## Attachment ownership

`rmuxd` treats each `attach_session` connection as an attachment. Attachments
are deliberately ephemeral: their identifiers are daemon-private and are not
client identities. This keeps the MVP safe after a laptop sleeps or a network
proxy disappears—there is no abandoned keyboard owner to clear manually.

Input and layout leases are independent. One attachment can type while a
different attachment owns PTY resizing. Requests to acquire a held lease leave
the requester attached as a viewer; they never force a takeover. A future
authenticated client identity may add reconnect grace periods and deliberate
takeover policies without changing session or stream semantics.

## Checkpoints

`rmuxd` continuously interprets raw output into terminal state. At bounded
output intervals, or before a prior checkpoint would no longer bridge retained
journal data, it creates a versioned checkpoint. A checkpoint captures the
visible terminal state and the parser state required to consume subsequent
output. It does not capture process memory, shell state, cwd, or scrollback
that the client has not retained locally.

The current `rmux` CLI restores a compatible checkpoint by writing its VT
restore stream to the local terminal. It reports a size mismatch but does not
resize the remote PTY. A richer future client may render the structured state
itself.
