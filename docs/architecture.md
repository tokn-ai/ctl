# Architecture

The monorepo contains two products with independent responsibilities:

- `rmux` provides persistent local terminal sessions.
- `ctl` exposes `rmux` sessions through an SSH-authorized remote command
  without reimplementing terminal persistence.

The current milestones make `rmux` usable through a desktop client that mixes
local and SSH targets, and through `ctl` from an SSH-authorized remote client.
The remote boundary intentionally exposes only the `rmux` service; generic
remote administration, files, jobs, port forwarding, and desktop control
remain out of scope.

## Process ownership

`rmuxd` is a per-user daemon. It owns every PTY and child process. A local
`rmux` client connects over per-user IPC and may disappear without affecting a
session.

`ctld connect` is a disposable SSH remote-command gateway:

```text
local:  rmux / rmux-app -> local IPC -> rmuxd -> PTY -> shell
remote: ctl / rmux-app -> OpenSSH -> ctld connect -> local IPC
                                                    -> rmuxd -> PTY -> shell
```

Each SSH channel gets a new `ctld connect` process. Ending that process drops
only its local attachment stream and must not affect a terminal session. If
`rmuxd` itself exits, an exact running PTY is not recoverable in the initial
architecture. Later disk-backed metadata may reconstruct explicitly
restartable tasks as a new process generation.

`ctld` owns neither terminal state nor session state. It relays raw bytes
between SSH stdin/stdout and one fixed local `rmuxd` endpoint. It does not
decode or reframe `rmux-proto`, and a remote peer cannot choose a local socket
path or service.

## Crate boundaries

- `rmux-proto`: versioned, platform-independent wire messages and framing.
- `rmux-core`: output journal and portable session-domain behavior.
- `rmux-client`: portable client-side protocol state, checkpoint restoration,
  attachment liveness, and terminal attachment behavior over an injected byte
  stream.
- `rmux-ipc`: per-user local endpoint selection and transport setup.
- `rmuxd`: local IPC, PTY/process ownership, and session coordination.
- `rmux`: canonical local CLI and reusable rmux command implementation.
- `rmux-app`: local/SSH Tauri/React terminal client in `apps/rmux`. Its Rust
  adapter composes `ctl-core` transport with `rmux-client`; its webview owns
  xterm rendering, viewport, and local scrollback.
- `ctl-core`: local/SSH transport selector. Its remote path owns an OpenSSH
  child, invokes one fixed `ctld connect` command, and exposes the resulting
  byte stream to the selected control-domain client.
- `ctld`: stateless SSH remote-command adapter for the fixed local `rmuxd`
  relay.
- `ctl`: control router. `ctl rmux` redirects the canonical rmux command
  surface locally by default or through an explicit OpenSSH destination.

OS-specific IPC and PTY implementation details must not enter `rmux-proto` or
`rmux-client`.
The initial IPC implementation targets Unix-domain sockets on macOS and Linux;
Windows named pipes will be a separate transport implementation.

`ctld` currently uses that Unix local endpoint and is therefore a Unix-only
host component. The OpenSSH transport and `rmux-proto` remain independent of
that endpoint so a future Windows implementation can preserve the same remote
behavior.

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
11. Leases belong to a logical attachment rather than one transport stream.
    Explicit detach releases them immediately; unexpected transport loss
    preserves them for a bounded reconnect grace, after which they are
    released while the shell continues.
12. OpenSSH owns host verification, encryption, and user authorization. `ctl`
    adds no network listener, forwarding, application key, or pairing state.
13. A `ctld connect` exit closes only its local stream and never terminates an
    `rmuxd` session. A replacement SSH channel may rebind the logical
    attachment using its memory-only token and renderer-applied raw sequence.
14. Optional shell-awareness metadata is advisory, memory-only session state.
    It is delivered as complete snapshots beside raw output, never inferred
    from rendered text or used for authorization, filesystem operations, or
    lease ownership.

An ordinary attaching client requests input only when no other attachment owns
it. It does not resize an existing PTY. A client must explicitly request the
layout lease before its terminal size is applied, so a small secondary client
cannot disturb an established desktop layout.

## Desktop client boundary

`rmux-app` is a client, not an embedded daemon. Every Tauri request carries an
explicit local or OpenSSH target. The backend composes `ctl-core` with
`rmux-client`, connects to the same per-user local endpoint as the CLI, and may
start a sibling `rmuxd`, but it does not link PTY, journal,
checkpoint-production, or session-lifetime logic into the app process.
Closing the window drops its attachment and leases while the daemon-owned
session continues.

The frontend persists selected remote target definitions and always includes
the local target. A read-only backend command discovers concrete
aliases from the user's OpenSSH config and recursive `Include` files for the
**+ Host** picker; wildcard and negated patterns are omitted, and discovery
never opens a connection. Selecting a suggestion promotes it to a configured
target. A new hostname can instead be saved as a managed, conflict-checked
block in `~/.ssh/config`, or as structured app-local hostname, user, port, and
identity-file-path fields. Config replacement uses a same-directory temporary
file, preserves existing file permissions, and refuses to write when alias
discovery is incomplete or the original changes during the operation.

App-local settings become separate, validated OpenSSH arguments and cannot
introduce arbitrary options or change the fixed `exec ctld connect` command.
The frontend queries configured targets concurrently and keys rows, tabs,
shell-state caches, mutations, and reconnect intent by `(target, session ID)`.
A failed target retains its last-known rows and reports a target-local error
without hiding successful targets. OpenSSH remains responsible for passwords,
key contents, proxies, host verification, and connection multiplexing. The app
adds no SSH prompt or credential surface.

The GUI omits a name when it creates a shell, so `rmuxd` applies the same
collision-safe `session-N` allocation used by every unnamed client. Its session
list merges authoritative geometry changes from the active attachment into the
matching row. **Disconnect** removes a selected open tab and preserves the
PTY; for the active tab it detaches the attachment, while an inactive tab is
already detached and is removed only from this window. **Close** is the
explicit one-shot kill operation and terminates the session for all attachments.

`Restart rmuxd` is a command-palette-only, destructive maintenance action. It
first preflights a separate owner-only local-control endpoint beside the normal
data endpoint. If an already-running older daemon does not support that
endpoint, the action returns `daemon_restart_unsupported` before detaching the
active view. After it accepts restart, `rmuxd` atomically stops admitting new
sessions and attachments, snapshots all live sessions, and requests their
termination. Existing data connections are then closed so a stalled client
cannot pin daemon drain; a connected attachment may observe its normal
session-ended event before that close. The GUI waits for both local endpoints
to drain, then starts a fresh daemon. It never unlinks a live endpoint or
guesses and signals a process ID.

The local-control endpoint is deliberately distinct from `rmux-proto` and is
never relayed by `ctld`; a remote `ctl` client cannot restart a daemon.
The backend records the target owned by its active attachment actor, so a
local restart detaches only a local attachment; a remote attachment in the
same window remains live. The frontend removes only local rows and tabs after
a successful or potentially destructive local restart.
Because `rmuxd` owns the PTYs, this is not a reconnect or session-preserving
recovery mechanism. If a restart has been accepted but the old daemon does not
drain in time, the action fails without force-stopping it. Raw-protocol version
compatibility remains the solution for an incompatible daemon, rather than
turning restart into a protocol-mismatch escape hatch. See
`docs/rmux-local-control.md` for the local-control protocol and lifecycle.

Concurrent CLI and GUI auto-start attempts may launch more than one daemon
candidate. Candidates serialize stale-socket inspection and replacement with
an owner-only endpoint lock, so a losing candidate cannot unlink a socket that
another candidate has just bound.

The backend exposes one window-scoped attachment actor through a Tauri channel.
Each presentation event has an opaque event ID, and only one checkpoint,
output, or geometry event crosses the webview bridge without an acknowledgement.
The frontend acknowledges output only after xterm's asynchronous write callback
and acknowledges checkpoints only after recreating a clean renderer and
feeding the normalized history and both checkpoint byte fields. This keeps the
bridge bounded without tying attachment heartbeats to webview rendering speed.

Attachment transitions are serialized per window by the backend. Opening a
different session detaches and awaits the previous actor before reserving its
replacement, which releases the previous attachment's leases without ending
its persistent shell session. The frontend therefore never asks a user to
manually detach before switching sessions.

The desktop normally uses one native window and one WebView. Its terminal tabs
are client-owned presentation state over a single window-scoped attachment
actor, so only the active tab is attached. Switching tabs atomically replaces
that attachment; inactive daemon-owned sessions continue running. **New Tab in
Current Folder** creates and attaches an auto-named persistent session without
reloading the WebView. The cwd is used only after an explicit user command and
only when shell awareness reports it; clients never infer a directory from
rendered terminal output. Closing a tab detaches its view without ending its
shell session. If the renderer is still starting, the GUI retains only the
latest selected attachment intent and connects it once the renderer is ready.
A failed attachment leaves the selected tab retryable rather than treating
local tab selection as proof that a daemon attachment exists.

Raw byte fields cross the Tauri boundary as base64. Every `u64` sequence and
revision crosses as a decimal string so JavaScript number precision cannot
corrupt a resume or acknowledgement boundary. Attachment and event IDs fence
late callbacks from a replaced renderer generation.

The desktop status line groups shell type and cwd separately from prompt
activity, the TUI hint, input ownership, layout behavior, and daemon-authoritative
PTY geometry. Lower-priority indicators collapse as the terminal pane narrows.
Connection and history-recovery warnings take priority, while raw sequence and
revision values remain diagnostics. The GUI does not request or serialize the
sensitive editable command buffer.

The renderer adopts daemon-authoritative PTY geometry. Selecting an existing
session never acquires layout ownership or changes its PTY grid. **Resize with
window** explicitly acquires the layout lease, measures the terminal container,
and sends debounced PTY resizes while that lease remains held; turning the mode
off releases the lease. A GUI-created session may request the lease with its
initial attachment because that GUI establishes the new session's layout.
Scrollback, scrolling, selection, and copy remain local xterm behavior and
generate no protocol viewport commands.

App actions are registered as stable frontend command IDs. The command palette,
toolbar, session list, and app-local shortcuts dispatch through that shared
registry, including dynamic commands for switching to a known session. The
webview intercepts only an exact enabled or disabled registered key combination
before xterm; unregistered keystrokes remain PTY input. Command search,
selection, and focus are client presentation concerns and do not add protocol
messages or daemon state.

The new-tab command uses `Cmd-T` on macOS and `Ctrl-Shift-T` on
Windows/Linux. It remains disabled with an explanatory palette reason until a
current shell-reported cwd is available.

On macOS, the native application menu routes `Cmd-W` to the shared detach-tab
command and `Cmd-E` to the shared close-session confirmation command. The
WebView does not also process those accelerators, preventing one keystroke from
dispatching twice. `Cmd-Q` remains the standard application quit and therefore
detaches without killing daemon-owned sessions. Windows and Linux use
`Ctrl-Shift-W` and `Ctrl-Shift-E` in the WebView so terminal `Ctrl-W` and
`Ctrl-E` remain PTY input.

## Attachment ownership

`rmuxd` treats each `attach_session` request as a logical attachment. Its
identifier remains daemon-private and is not a client identity. The client
receives only a random, memory-only token that may rebind a replacement
transport during a bounded grace period.

Input and layout leases are independent. One attachment can type while a
different attachment owns PTY resizing. Requests to acquire a held lease leave
the requester attached as a viewer; they never force a takeover. Only
possession of an attachment's token may supersede that attachment's stale
transport generation; it does not displace another logical attachment.

To prevent a sleeping or half-open client from pinning either capability,
`rmuxd` negotiates a heartbeat cadence and liveness deadline during the
handshake. Only inbound client activity renews that deadline after the initial
attachment transfer. That transfer has its own finite delivery deadline, since
a client learns the heartbeat cadence only after `attached` and `rmuxd`
serially delivers initial replay before it can process queued heartbeats. A
silent transport is closed, but its logical attachment remains resumable for
one bounded reconnect interval. Possession of the attachment token immediately
supersedes the stale transport generation while preserving its leases. An
explicit detach or expired grace releases the leases; the PTY, shell, journal,
and checkpoint state remain intact.

## Remote control boundary

`ctl` connects directly to the current user's owner-only `rmuxd` data endpoint
by default. Global `--host`/`-H` instead invokes the system OpenSSH client with
PTY allocation and all forwarding disabled, an OpenSSH destination supplied
by the user, and the fixed remote command `exec ctld connect`. OpenSSH
configuration owns host verification, user authentication, proxying, and
healthy-connection multiplexing. `ctl` never disables host-key checking,
enables agent forwarding, or accepts an arbitrary remote command.

`ctld connect` has no network listener, persistent state, identity registry,
or service selector. It writes one fixed readiness marker, after which SSH
stdin/stdout carries raw `rmux-proto`; diagnostics use stderr. The helper
connects only to the current user's fixed `rmuxd` data endpoint and cannot
reach its owner-only maintenance endpoint. Its authority is exactly that of
the already SSH-authenticated operating-system account.

Reconnect state stays inside `rmuxd`. Its opaque attachment tokens are random,
memory-only, session-scoped credentials for rebinding a replacement stream;
they are not device identities or substitutes for SSH authorization. See
`docs/ctl-protocol.md` for the transport contract and `docs/remote-mvp.md` for
setup.

## Shell awareness

`rmuxd` can track a shell descriptor, cwd display string, prompt phase,
optional editable command buffer/cursor, optional bounded running-command
summary, and an alternate-screen presentation hint. This is not terminal
emulation and does not introduce viewport commands: clients still own
scrolling, selection, search, and rendering.

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

The live edit buffer and running-command summary may contain secrets. They are
never in `SessionInfo` or the session list, and `get_shell_state` always
redacts both. An attachment must opt in to each separately and currently own
the input lease before `rmuxd` sends it. The shipped `zsh` v2 integration
replaces editable text with a bounded running-command summary before command
execution; `bash` does not advertise either live-editing or running-command
capability. `rmux attach` and `ctl rmux attach` are raw terminal presenters and
intentionally do not request or print either value.

Version-2 FIFO reports preserve the version-1 nine-field NUL-delimited wire
shape. Their active-text fields are phase-exclusive: prompt phases carry an
editable buffer/cursor while `running` carries only the non-editable summary.
The daemon still accepts version-1 reports, which retain their old
editable-buffer-only semantics.

`tui_hint` means only that DEC alternate-screen modes were observed. It is not
a classification of the child process: some TUIs do not use alternate screen,
and normal applications can use it. It may inform a client overlay but never
changes input or layout ownership.

## Checkpoints

`rmuxd` continuously interprets raw output into terminal state. At bounded
output intervals, or before a prior checkpoint would no longer bridge retained
journal data, it creates a versioned checkpoint. A checkpoint captures the
live terminal state and the parser state required to consume subsequent output.
At that exact raw sequence, `rmuxd` also captures a bounded full replacement of
normalized logical lines above the live grid. It does not capture process
memory, shell-awareness state, or cwd. A current shell-awareness snapshot
travels separately with `attached` and later state-change messages.

The current `rmux` CLI restores a compatible checkpoint by writing its VT
restore stream to the local terminal. It reports a size mismatch but does not
resize the remote PTY. Because a native terminal cannot atomically replace its
outer scrollback, the CLI does not inject the normalized history snapshot. The
GUI recreates its owned renderer and restores both history and live state.

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
4. Seed the paired normalized history above the new live grid. The history and
   checkpoint sequences must match; neither is valid without the other.
5. Feed `payload`, then `input_prefix`, byte-for-byte through the same terminal
   input path that will consume later raw output. `input_prefix` may be an
   incomplete UTF-8 prefix, so it must not be converted to text or decoded
   separately. Incomplete terminal-control parser state is represented by the
   checkpoint payload itself.
6. Treat `checkpoint.sequence` as the next raw-stream offset after the
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

The GUI has an acknowledgement boundary between its Rust transport/controller
and the asynchronous webview terminal emulator. It tracks two different
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
The controller sends that progress to `rmuxd` as coalesced
`presentation_applied` delivery credit. The daemon stops sending presentation
events at the negotiated byte/event window instead of closing the transport.
Heartbeats and control messages remain live while output is paused.

Delivery progress and safe resume state remain distinct. An incompatible
checkpoint may be acknowledged to replenish the daemon's window while the
client keeps its safe resume cursor absent. On reconnect, a GUI uses its local
safe sequence only when it retained a compatible renderer; otherwise it omits
`resume_from` and begins from a checkpoint/history replacement.

If a presenter observes a geometry transition but cannot adopt its PTY grid,
it must acknowledge that incompatibility locally rather than treating the
transition as applied. It may continue displaying bytes, but its reconnect
cursor remains absent until it has applied a later checkpoint. The raw stdio
adapter follows this policy because printing an updated size warning does not
resize the user's terminal.

### Authoritative history and client presentation

The daemon owns two bounded terminal representations: a mutable live emulator
and normalized complete logical lines above its grid. Raw PTY output remains the
ordered delta and short replay journal. At every checkpoint boundary, the
daemon snapshots history and live state together; subsequent raw output evolves
both server and client emulators from that boundary.

Historical lines have already interpreted terminal controls, merge soft wraps,
and exclude alternate-screen output. Version 1 stores text only; style runs are
deliberately deferred. `RIS` and erase-saved-lines start a new generation. A
resize may move the live/history boundary, so version 1 sends a full history
replacement rather than stable incremental line IDs.

Selection ranges, search indexes, viewport position, and any retention beyond
the daemon bound remain client-local presentation data. Scrolling never becomes
a daemon viewport command. Applying a checkpoint recreates the GUI renderer,
seeds its paired history above the grid, restores the live payload, and only
then applies output deltas. If `history_gap` is true, the UI marks the missing
oldest portion even though the live screen is authoritative.

### GUI foundation acceptance tests

Tab, split, and remote-host UX must preserve the following attachment-layer
behavior, proven with a test renderer and, where possible, the chosen terminal
emulator in headless mode:

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
- Restoring a checkpoint/history pair places normalized logical lines above the
  live grid without duplicating them in the viewport.
- An ordered daemon geometry update changes every attached renderer's grid at
  its stream boundary without causing a viewing client to send `resize` or
  acquire layout ownership.
- A stalled webview exhausts presentation credit instead of closing the
  transport; the Rust-to-webview queue stays bounded and heartbeats continue.
