# rmux local control protocol

This document specifies the owner-only maintenance endpoint exposed by a local
Unix `rmuxd`. It is intentionally separate from `rmux-proto`: the latter is a
terminal-session protocol and may be relayed by `ctld`; this protocol controls
the daemon process itself and is never remote.

## Endpoint and access boundary

For a configured data endpoint named `PATH`, the local-control endpoint is
`PATH.control`. Both endpoints live in the same validated per-user runtime
directory, are bound while the same startup lock is held, and are mode `0600`.
`rmuxd` removes each endpoint only if its own listener inode still occupies the
path.

`ctld` connects only to the data endpoint. It must not discover, connect to,
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
         restart_supported: true
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
