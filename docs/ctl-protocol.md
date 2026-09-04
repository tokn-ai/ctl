# ctl SSH transport

`ctl` carries a versioned `rmux-proto` or `task-proto` stream through an OpenSSH
remote command. There is no separate network listener, TLS identity, pairing format,
or outer application-authentication protocol. A fixed `ctl-ssh-v1` readiness
marker precedes the raw stream so startup output cannot be mistaken for a
service protocol frame. The selected service performs its own protocol handshake
after this transport marker.

## Connection command

`ctl rmux` uses the canonical rmux command implementation with a ctl-selected
transport. When global `--host`/`-H` selects a remote target, every one-shot
request or interactive attachment starts the system OpenSSH client with the
equivalent fixed argument sequence for the default Unix remote platform:

```text
ssh -T \
  -o ClearAllForwardings=yes \
  -o ForwardAgent=no \
  -o ForwardX11=no \
  -o PermitLocalCommand=no \
  -o RemoteCommand=none \
  -- <destination> exec ctl-agent connect
```

With `--remote-platform windows`, the suffix is `ctl-agent.exe connect` instead
of `exec ctl-agent connect`. This is an enumerated server-platform choice, independent
of the client OS, and currently requires the Windows server's default cmd.exe
shell. Both choices retain the same SSH options and binary protocol. Omitting
`--remote-platform` preserves Unix behavior; the option requires `--host`.

`ctl task` uses the same OpenSSH settings and target selection with an explicit
fixed service suffix:

| Domain | Unix remote command | Windows remote command |
| --- | --- | --- |
| rmux | `exec ctl-agent connect` | `ctl-agent.exe connect` |
| task | `exec ctl-agent connect --service task` | `ctl-agent.exe connect --service task` |

The service is an enum selected by the command domain. Neither a socket path nor
an arbitrary service or shell command is accepted from the client.

`<destination>` is an OpenSSH destination or `Host` alias. Host-key checking,
user authentication, certificates, agents, proxy jumps, ports, and connection
multiplexing remain OpenSSH configuration. `ctl` never disables host-key
verification, enables agent forwarding, creates a forwarding, or accepts a
user-controlled remote command.

Without `--host`, transport-requiring `ctl rmux` commands connect directly to
the current user's owner-only local `rmuxd` endpoint and do not start SSH or
`ctl-agent`. The `rmux-proto` request, attachment, detach, and reconnect behavior
above that transport is shared by both targets.

Local `ctl task` commands similarly use the per-user taskd endpoint. Remote task
create/start and other multi-request operations reopen the task stream on the
same target. `task attach` first obtains the interactive session ID from taskd,
then opens the rmux transport on that target. A remote daemon's socket path is
metadata, never a client-side endpoint.

OpenSSH may reuse a healthy configured control master. A broken SSH transport
cannot resume an existing channel. For rmux attachments, `ctl` starts a
replacement channel and uses the `rmux-proto` attachment token described below.

## Remote command

`ctl-agent connect` is a disposable, stateless process launched once per SSH
channel. The default service connects to the fixed per-user `rmuxd` data endpoint;
`--service task` connects to the fixed per-user `taskd` endpoint. It starts only
the selected absolute-path companion daemon when necessary, writes
`ctl-ssh-v1\n` to stdout, then copies raw bytes in both directions:

```text
SSH stdin  -> ctl-agent connect -> rmuxd data endpoint
SSH stdout <- ctl-agent connect <- rmuxd data endpoint

SSH stdin  -> ctl-agent connect --service task -> taskd endpoint
SSH stdout <- ctl-agent connect --service task <- taskd endpoint
```

Completion or failure of either copy direction ends the relay and closes its
local IPC stream. Diagnostics use stderr exclusively. The remote command accepts
no arbitrary local socket, forwarded address, or service outside the rmux/task
enum. The sibling owner-only `rmuxd` maintenance endpoint is never exposed.
Taskd accesses that endpoint locally for managed interactive-session lifecycle;
the gateway itself cannot route to it.

The endpoint is a Unix socket or an owner-restricted Windows named pipe.
Windows discovers `rmuxd.exe` or `taskd.exe` beside `ctl-agent.exe`. Auto-start
detaches the daemon from the console and requests breakaway from the SSH job,
with null standard streams. If job policy disallows breakaway, startup fails explicitly. The
SSH gateway itself remains disposable and the maintenance pipe remains local.

Non-interactive remote shell startup files must not write to stdout. Such bytes
precede the readiness marker, so `ctl` rejects the connection with a focused
startup-output error instead of feeding them to the selected protocol; stderr
output is safe.

## Authorization boundary

SSH authenticates the device and user. `ctl-agent` adds no identity, authorization,
or capability registry. It runs with the SSH account's existing authority and
can reach only that account's fixed rmux data or task endpoint.

This does not grant a successfully authenticated SSH account new local
authority: the same account can already connect to those owner-only sockets.
Task access permits registering and running commands with that account's
authority. Deployments using an SSH forced command must explicitly allow the
task form as well as the rmux form to enable remote task execution. The Docker
target allowlists exactly the two Unix commands above and never evaluates the
supplied SSH command as shell input.

## Reconnect lifecycle

`rmuxd`, not `ctl-agent`, owns reconnect state. A successful initial attachment
returns an opaque memory-only token. After an unexpected transport loss,
`rmuxd` retains that logical attachment and its input/layout leases for the
negotiated liveness interval, 30 seconds by default.

A replacement SSH channel sends `resume_attachment` with the token and the
renderer-applied raw output sequence. A valid token:

- reuses the same daemon-private attachment ID;
- immediately supersedes the prior transport generation, including a
  half-open one;
- preserves the attachment's existing input and layout leases;
- independently replays output from the renderer's requested sequence.

An explicit `detach` releases the attachment immediately and is confirmed by
`detached`. If the reconnect grace expires, `rmuxd` forgets the token and
releases both leases. Tokens are random, session-scoped, never persisted or
logged, and disappear with `rmuxd`; an invalid or expired token receives
`attachment_resume_rejected`, after which a client may open a new attachment
normally.

The PTY, shell, journal, checkpoints, and shell-awareness state remain owned by
`rmuxd` throughout. No keyboard input is replayed after reconnect.

Task lifecycle requests and background log streams do not inherit rmux's
attachment tokens or automatic resume. A closed task channel leaves the daemon
and its processes running. Clients can inspect task status and reopen logs;
`task logs <task> --after <sequence>` selects a cursor within the retained in-memory
log. Interactive task attachment uses the rmux lifecycle above.

See [Proposal 0006](proposals/0006-remote-tasks.md) for the service boundary and
[remote setup](remote-mvp.md) for packaging, working directories, and persistence.
