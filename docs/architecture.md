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
resize the remote PTY. A richer future client may apply a compatible checkpoint
through its own terminal renderer.

### Renderer-safe checkpoint application

A checkpoint is an initialization program for a clean terminal renderer, not
an idempotent screen delta. In particular, the current `rmux_vt_state` version
1 payload is an `avt` VT dump. It recreates the represented buffers and modes,
but does not promise to erase unrelated content or parser state already held by
the receiving renderer.

A graphical presenter that supports this format must therefore:

1. Verify the format and format version before changing its live presentation.
2. Stop applying later output while the restore is in progress, with a bounded
   local queue.
3. Discard or recreate its terminal emulator at the checkpoint's
   `terminal_size`; this resets both screen buffers and its input decoder.
4. Feed `payload`, then `input_prefix`, byte-for-byte through the same terminal
   input path that will consume later raw output. `input_prefix` may be an
   incomplete UTF-8 prefix, so it must not be converted to text or decoded
   separately. Incomplete terminal-control parser state is represented by the
   checkpoint payload itself.
5. Treat `checkpoint.sequence` as the next raw-stream offset after the
   restored state. It is not derived from the length of either checkpoint
   field. Apply only output beginning at that offset after the restore.

The current raw stdio presenter establishes that clean state with a terminal
reset and full-screen clear before writing the version-1 stream. A graphical
client should instead recreate its terminal model; it must not depend on a
particular physical terminal's reset behavior.

The renderer must not send `resize` merely to match the checkpoint dimensions.
PTY geometry remains layout-owner controlled; a viewing client renders at the
daemon's geometry and may use its own viewport scaling or scrolling around
that grid. An unsupported checkpoint is an explicit compatibility failure: the
client must not advance its resume position or claim that the visible terminal
state was restored.

### GUI render progress and reconnect

The protocol has no per-output render acknowledgement. A GUI nevertheless
needs a local acknowledgement boundary between its Rust transport/controller
and an asynchronous webview terminal emulator. It tracks two different
positions for each attachment:

- `received_next_sequence`: the end of validated output accepted from the
  transport;
- `applied_next_sequence`: the end of output whose effects the renderer has
  completed, after the renderer's write callback or equivalent completion
  signal.

Only `applied_next_sequence` is eligible for the next `attach_session`
`resume_from` value. Receiving an event in the webview, placing it in a queue,
or starting an asynchronous terminal write is not enough. A checkpoint becomes
applied only after the fresh renderer has accepted its `payload` and
`input_prefix`; its applied position is then exactly `checkpoint.sequence`.
The controller keeps heartbeats independent of renderer progress, and bounds
any bridge queue; a renderer that cannot catch up is detached and resumed from
its last applied position rather than silently dropping raw bytes.

This acknowledgement is local client state, not a new daemon-owned cursor and
not a protocol message. `rmuxd` continues to own only its bounded raw journal
and checkpoints. On a reconnect, a GUI may use its locally applied sequence
when it retained a valid renderer; otherwise it omits `resume_from` and begins
from a compatible checkpoint.

If a presenter observes a geometry transition but cannot adopt its PTY grid,
it must acknowledge that incompatibility locally rather than treating the
transition as applied. It may continue displaying bytes, but its reconnect
cursor remains absent until it has applied a later checkpoint. The raw stdio
adapter follows this policy because printing an updated size warning does not
resize the user's terminal.

### Client-local scrollback

The daemon's raw output journal is the resumable source of truth. Rendered
lines, selection ranges, search indexes, and scroll position are client-local
presentation data. A GUI may keep an in-memory or explicitly configured local
history cache, but it must record the raw sequence coverage that produced it
and must never turn scrolling into a daemon viewport command.

Applying a checkpoint resets the terminal emulator's own scrollback along with
its visible buffers. A separate local archive may keep older rendered output,
but it must remain visibly separate from the newly restored live terminal and
must avoid replay duplicates. If `history_gap` is true, the UI must mark a
discontinuity rather than presenting its prior local lines and the restored
screen as uninterrupted remote history. A checkpoint restores the current
terminal state; it does not reconstruct discarded remote scrollback.

The daemon's terminal parser is only a checkpoint producer, not a second
scrollback service. Its rendered scrollback must remain bounded independently
of the raw journal; the GUI owns the user-facing retention policy.

### GUI foundation acceptance tests

Before adding tab, split, or remote-host UX, the GUI attachment layer must
prove the following behavior with a test renderer and, where possible, the
chosen terminal emulator in headless mode:

- Restoring a version-1 checkpoint into a deliberately dirty renderer removes
  stale state, applies the checkpoint at its own dimensions, and matches the
  expected screen/cursor/mode state.
- A checkpoint followed by split UTF-8 input preserves a single byte decoder
  across `payload`, `input_prefix`, and later output; a checkpoint whose
  payload contains partial terminal-control parser state accepts its later
  continuation correctly.
- A delayed renderer acknowledgement never advances `resume_from`; reconnect
  from the last applied sequence loses or duplicates no raw output.
- Recovery with `history_gap` restores the live screen while clearly preserving
  the local-history discontinuity.
- An ordered daemon geometry update changes every attached renderer's grid at
  its stream boundary without causing a viewing client to send `resize` or
  acquire layout ownership.
- A stalled webview cannot create an unbounded Rust-to-webview queue, and
  heartbeats continue while bounded recovery is in progress.
