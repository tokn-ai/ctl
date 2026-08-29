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
- `rmux-ipc`: per-user local endpoint selection and transport setup.
- `rmuxd`: local IPC, PTY/process ownership, and session coordination.
- `rmux`: local command-line client and interactive terminal presentation.

OS-specific IPC and PTY implementation details must not enter `rmux-proto`.
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

An attaching client does not resize an existing PTY. Explicit layout ownership
will be introduced before automatic resize behavior, so a small secondary
client cannot disturb the session's established layout.

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
