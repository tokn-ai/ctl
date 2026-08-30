# Architecture

The monorepo contains two products with independent responsibilities:

- `rmux` provides persistent local terminal sessions.
- `ctl` provides authenticated remote device control and will expose `rmux`
  sessions without reimplementing terminal persistence.

The current milestone makes `rmux` usable through `ctl` from a paired remote
client. It intentionally exposes only the `rmux` service; generic remote
administration, files, jobs, port forwarding, and desktop control remain out
of scope.

## Process ownership

`rmuxd` is a per-user daemon. It owns every PTY and child process. A local
`rmux` client connects over per-user IPC and may disappear without affecting a
session.

`ctld` is an authenticated gateway with an independent lifecycle:

```text
local:  rmux -> local IPC -> rmuxd -> PTY -> shell
remote: ctl  -> network   -> ctld  -> local IPC -> rmuxd -> PTY -> shell
```

`ctld` and `rmuxd` have independent lifecycles. Restarting `ctld` must not
affect a terminal session. If `rmuxd` itself exits, an exact running PTY is not
recoverable in the initial architecture. Later disk-backed metadata may
reconstruct explicitly restartable tasks as a new process generation.

`ctld` owns neither terminal state nor session state. After an authenticated
client opens the `rmux` service, it relays raw bytes between its TLS connection
and one fixed local `rmuxd` endpoint. It does not decode or reframe
`rmux-proto`, and a remote peer cannot choose a local socket path.

## Crate boundaries

- `rmux-proto`: versioned, platform-independent wire messages and framing.
- `rmux-core`: output journal and portable session-domain behavior.
- `rmux-client`: portable client-side protocol state, checkpoint restoration,
  attachment liveness, and terminal attachment behavior over an injected byte
  stream.
- `rmux-ipc`: per-user local endpoint selection and transport setup.
- `rmuxd`: local IPC, PTY/process ownership, and session coordination.
- `rmux`: local command-line adapter that starts/connects to `rmuxd` over IPC.
- `ctl-proto`: versioned outer control messages for pairing and service
  selection. It is independent of terminal and operating-system details.
- `ctl-core`: portable client identity, pinned TLS connection, and outer
  control handshake. It returns an injected byte stream for `rmux-client`.
- `ctld`: device-local identity/authorization registry, TLS endpoint, and the
  fixed local `rmuxd` relay.
- `ctl`: remote command-line adapter and owner-only local client state.

OS-specific IPC and PTY implementation details must not enter `rmux-proto` or
`rmux-client`.
The initial IPC implementation targets Unix-domain sockets on macOS and Linux;
Windows named pipes will be a separate transport implementation.

`ctld` currently uses that Unix local endpoint and is therefore a Unix-only
host component. `ctl-proto`, `ctl-core`, and the remote TLS control transport
remain platform-independent so a future Windows local endpoint can preserve
the same remote protocol.

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
11. Leases are connection-bound and liveness-bounded. They are released when
    their attachment detaches, disconnects, or stops proving liveness, while
    the shell session itself continues.
12. `ctld` authorization is explicit device/client identity, not Tailscale
    network location. Tailscale remains the expected private reachability
    layer.
13. A `ctld` shutdown closes its local attachments but never terminates an
    `rmuxd` session; a later authenticated connection attaches by durable
    session ID and raw output sequence.
14. Optional shell-awareness metadata is advisory, memory-only session state.
    It is delivered as complete snapshots beside raw output, never inferred
    from rendered text or used for authorization, filesystem operations, or
    lease ownership.

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

To prevent a sleeping or half-open client from pinning either capability,
`rmuxd` negotiates a heartbeat cadence and liveness deadline during the
handshake. Only inbound client activity renews that deadline after the initial
attachment transfer. That transfer has its own finite delivery deadline, since
a client learns the heartbeat cadence only after `attached` and `rmuxd`
serially delivers initial replay before it can process queued heartbeats. A
silent attachment is closed and loses its leases, but its PTY, shell, journal,
and checkpoint state remain intact. A reconnecting client may request an
unheld former lease again; it never revokes a live attachment's ownership.

## Remote control boundary

`ctld` has a per-device self-signed TLS certificate and a private
authorization registry. A pairing invitation carries the device's pinned
public certificate, stable synthetic TLS name, endpoint, client label, and a
short-lived one-time bearer token. The device stores only a SHA-256 digest of
that token. A client generates its own Ed25519 identity locally and signs a
fresh server challenge on every connection; the private client key never
leaves the client device.

The initial implementation keeps certificate pinning and client challenge
signatures separate rather than using a transport client certificate. This
keeps protocol authorization explicit and lets the authorized-key registry
drive future revocation and capabilities without relying on Tailnet location.
The only current capability is `rmux_tunnel`.

`ctld serve` requires an explicit non-wildcard address. In normal use that is
a device's concrete Tailscale IP address; it never defaults to a public
all-interface management listener. Device state directories and
private keys are owner-only on Unix. See `docs/ctl-protocol.md` for the exact
outer handshake and upgrade boundary, and `docs/remote-mvp.md` for a safe
first-use flow.

## Shell awareness

`rmuxd` can track a shell descriptor, cwd display string, prompt phase,
optional editable command buffer/cursor, and an alternate-screen presentation
hint. This is not terminal emulation and does not introduce viewport commands:
clients still own scrolling, selection, search, and rendering.

On the current Unix implementation, an opt-in shell integration writes bounded
full snapshots to a unique owner-only FIFO supplied as
`RMUX_SHELL_STATE_PIPE`. The integration removes that environment variable and
opens the FIFO only for each report, so commands it executes do not inherit a
reporter capability. The FIFO is not an `rmux-proto` client endpoint. That
keeps the reporter's separate typed-buffer records out of raw journal,
checkpoints, replay, and future journal persistence; normal terminal echo is
still canonical raw output. Reports are advisory because a child process can
lie; `rmuxd` assigns the revision and output-sequence correlation itself, and
coalesces/rate-limits reports before they can contend with PTY ingestion.

The live edit buffer may contain secrets. It is never in `SessionInfo` or the
session list, and `get_shell_state` always redacts it. An attachment must opt
in and currently own the input lease before `rmuxd` sends it. The shipped
`zsh` integration clears it before command execution; `bash` v1 does not
advertise live edit-buffer capability. `rmux attach` and `ctl shell` are raw
terminal presenters and intentionally do not request or print the buffer.

`tui_hint` means only that DEC alternate-screen modes were observed. It is not
a classification of the child process: some TUIs do not use alternate screen,
and normal applications can use it. It may inform a client overlay but never
changes input or layout ownership.

## Checkpoints

`rmuxd` continuously interprets raw output into terminal state. At bounded
output intervals, or before a prior checkpoint would no longer bridge retained
journal data, it creates a versioned checkpoint. A checkpoint captures the
visible terminal state and the parser state required to consume subsequent
output. It does not capture process memory, shell-awareness state, cwd, or
scrollback that the client has not retained locally. A current shell-awareness
snapshot travels separately with `attached` and later state-change messages.

The current `rmux` CLI restores a compatible checkpoint by writing its VT
restore stream to the local terminal. It reports a size mismatch but does not
resize the remote PTY. A richer future client may render the structured state
itself.
