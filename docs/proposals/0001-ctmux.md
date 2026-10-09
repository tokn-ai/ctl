# Proposal 0001: Persistent terminal sessions with ctmux

- Status: Implemented
- Created: 2026-09-04
- Updated: 2026-10-09

## Summary

`ctmux` provides persistent local terminal sessions. A per-user `ctmuxd` owns each
session's pseudo-terminal (PTY), child process, output stream, and attachment
state. Clients may disconnect and reconnect without terminating the session.

## Motivation

A terminal process should continue when its CLI or desktop viewer closes, its
transport fails, or its user moves between clients. Multiple clients should be
able to observe one terminal without accidentally competing for input or
changing its layout.

## Design

`ctmuxd` is the authority for terminal sessions. It creates the PTY and child
process, reads and journals ordered output, forwards authorized input, applies
terminal geometry, observes process exit, and retains session state while no
client is attached. The current daemon and its journals are memory-backed.

The `ctmux` CLI is the canonical local command surface. It can create, list,
inspect, attach to, detach from, and terminate sessions. The desktop ctmux app is
another client of the daemon rather than an embedded daemon. The current
`ctmux` command opens the shared TUI by default; scripts use detached creation
explicitly. Raw attachment remains available. See
[Proposal 0014](0014-shared-panes-tui.md) for this CLI evolution and shared panes.

An attachment is a viewer with two independently leased capabilities:

- the input lease permits terminal input;
- the layout lease permits PTY resizing.

Neither attaching nor viewing implicitly resizes a session. Transport loss
temporarily preserves a logical attachment and its leases so a replacement
connection can resume it. Explicit detach releases the attachment immediately
and does not terminate the session. Explicit kill terminates the session for
all clients.

The desktop remembers observed terminal dimensions and last-seen time for
presentation ([PR #55](https://github.com/tokn-ai/ctl/pull/55)). Restoring these
observations does not resize a PTY, authenticate a host, or prove that a session
is alive.

Raw PTY bytes are the canonical output record. Bounded checkpoints and logical
history allow a renderer to recover without replaying an arbitrarily large
journal. Optional shell-awareness metadata is advisory and separate from raw
terminal output.

## Invariants

1. `ctmuxd` owns every ctmux PTY and its child process.
2. Client disconnect, transport loss, and detach do not terminate a session.
3. Output ordering uses session-global, monotonically increasing byte offsets.
4. Input and layout ownership are independent, explicit leases.
5. An attachment cannot implicitly take a lease from another attachment.
6. PTY geometry changes are authoritative session events visible to all
   attachments.
7. Shell metadata does not replace terminal output or grant authority.
8. Clients use a versioned protocol independent of the local IPC transport.

## Out of scope

This proposal does not define general background jobs, task definitions,
restart policies, dependency management, or boot-time services. A future task
system may request interactive executions from ctmux, but every PTY remains
owned by `ctmuxd`.

## Unresolved questions

None for the implemented boundary. Client-local persistence and paged daemon
history are recorded in [Proposal 0013](0013-terminal-history.md); shared views
and the terminal UI are recorded in [Proposal 0014](0014-shared-panes-tui.md).
The daemon remains memory-backed. Windows transports are recorded below and in
[Proposal 0004](0004-windows-ssh.md).

## Detailed specifications

- [Architecture](../architecture.md)
- [ctmux protocol](../ctmux-protocol.md)
- [ctmux local control protocol](../ctmux-local-control.md)
- [Desktop workspace persistence](../ctmux-workspace.md)


## Windows local backend

Windows local sessions use ConPTY through `portable-pty`. Ctmuxd remains the
owner of the terminal, attached processes, raw output journal, checkpoints,
and leases. The local CLI and local `ctl ctmux` route over owner-restricted
named pipes. The maintenance endpoint remains separate from the data protocol.

Session shutdown closes ConPTY on a thread separate from the output reader,
then publishes the exit event after final output is drained. Native Windows
CI covers reconnect, resize, exit output, and cooperative daemon restart.
Windows shell reporting, native process metadata, desktop transport, and SSH
routing are separate from this local terminal backend.
