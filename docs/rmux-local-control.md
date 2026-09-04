# rmux local control protocol

This document specifies the owner-only local-control endpoint exposed by rmuxd
on Unix and Windows. It is separate from `rmux-proto`: the latter carries
terminal sessions and may be relayed by `ctl-agent`; local control supports daemon
restart and task-owned session lifecycle operations and is never relayed.

## Endpoint and access boundary

For a configured data endpoint named `PATH`, the local-control endpoint is
`PATH.control`. Both endpoints live in the same validated per-user runtime
directory, are bound while the same startup lock is held, and are mode `0600`.
`rmuxd` removes each endpoint only if its own listener inode still occupies the
path. On Windows, both endpoints are local named pipes with an owner-only DACL
and remote clients rejected; the control pipe appends `.control` to the data
pipe name. Both first instances are bound under the same startup lock.

`ctl-agent` connects only to the data endpoint. It must not discover, connect to,
or relay the control endpoint. A future remote daemon lifecycle feature needs a
separately authorized `ctl` service; it must not be added to raw `rmux-proto`.

## Version 1 framing

The protocol uses length-prefixed JSON frames: an unsigned four-byte
big-endian payload length followed by UTF-8 JSON. A frame may be at most 64
KiB. Each connection has one handshake and may then request one operation.

```text
client                                      local rmuxd control endpoint
------                                      ----------------------------
handshake { protocol_version: 1 }       ->
       handshake_accepted {
         protocol_version: 1,
         restart_supported: true,
         managed_sessions_supported: true
       }                                  <-
restart_daemon                         ->
       restart_accepted { terminated_sessions } <-
```

Errors are structured as:

```text
error { code, message }
```

Version 1 error codes are `invalid_request`, `protocol_version_mismatch`,
`restart_unsupported`, `restart_in_progress`, and `internal`.

## Coordinated restart semantics

The GUI preflights the control handshake while its current attachment remains
live. If the normal data endpoint is reachable but the control endpoint is
absent or incompatible, it returns `daemon_restart_unsupported` and does not
detach the active view. If no daemon is reachable, the GUI may start a new one
normally; there is nothing to terminate.

After `restart_daemon` is accepted, `rmuxd` uses one lifecycle gate to stop new
session creation and attachment admission, snapshot every existing session,
and request every session's termination. It then closes existing data
connections, so a stalled client cannot pin the shutdown; a connected
attachment may observe its usual terminal `session_ended` event before that
close. The daemon exits naturally only after all sessions and data/control
connections have drained. The caller waits until both socket endpoints are
unavailable before it starts the replacement daemon.

No participant sends a process signal or unlinks a live socket. Before the
restart is accepted, failure is non-destructive. After acceptance, sessions are
deliberately being terminated; a later drain or replacement failure is reported
without forcing the old daemon down. Since `rmuxd` owns the PTYs, restart cannot
preserve an interactive session and is not a substitute for protocol-version
compatibility.


## Managed interactive sessions

The additive `managed_sessions_supported` handshake capability defaults to
false when omitted by older daemons. Taskd requires it before registering an
interactive run intent. Existing restart clients may ignore this field.

A lifecycle request is a `manage_session` frame with `task_id`, `run_id`,
`expected_instance`, and a nested `operation`. Task and run IDs must be UUIDs.
Operations are:

| Operation object | Behavior |
| --- | --- |
| `{ "operation": "status" }` | Read the instance UUID and optional session lifecycle. |
| `{ "operation": "start", "command": { "program": "…", "arguments": [] }, "working_directory": null }` | Create a PTY once for this task/run pair or return its existing session. |
| `{ "operation": "stop" }` | Terminate the session; an absent run is retired without launching it. |
| `{ "operation": "release" }` | Acknowledge a finished result; a running session cannot be released. |

Every response is `managed_session` with `instance_id` and either `session: null`
or a session containing `session_id`, `running`, and nullable `exit_code`.
Status permits a null expected instance. Mutations require the exact instance
UUID returned by status, and execute under the same gate as cooperative restart.
A replacement rmuxd must never accept creation or termination for an old instance.
Repeated start returns the same session even after it has exited, until release.
Released or cancelled run IDs reject later start requests.

Taskd saves run intent and the instance UUID before sending start. It polls
lifecycle metadata without reading, duplicating, or forwarding terminal output.
A restart of taskd can finish pending creation or recover the original session
by task/run identity. Uncertain responses retain an active `unknown` run and
block a new start; an absent or replaced rmuxd fails the run. Reconciliation
never auto-starts rmuxd. Only an explicit task start may launch it.

Rmuxd keeps ended managed sessions and their exit results until taskd persists
its result and sends release. Unacknowledged outcomes keep rmuxd alive even if
its ordinary session list is empty. Outcome retention lasts for that rmuxd
instance, not across its crash or replacement. An explicit cooperative restart
terminates live managed sessions and discards retained outcomes so they do not
prevent daemon drain. Taskd then records the loss with an unknown exit code.

Released run IDs retain small tombstones to prevent delayed creation from
resurrecting an old run. Each rmuxd instance accepts at most 4096 distinct managed
run IDs; existing runs remain controllable at that limit. After stopping tasks,
restart rmuxd to reset the limit. Tombstones alone do not prevent normal idle
exit. Taskd retains the backend endpoint in each run so changing its runtime
configuration does not silently redirect recovery to another endpoint.

The task CLI uses protocol version 3. On-disk task state remains schema version
1 with optional interactive metadata; older background records are accepted.
State replacement is atomic, with recovery designed for process crashes; no
power-loss durability guarantee is added here. The latest completed result is
persisted before release, and acknowledgement is retried after taskd restart.
