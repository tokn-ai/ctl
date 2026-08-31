# ctl SSH transport

`ctl` carries the versioned `rmux-proto` stream through an OpenSSH remote
command. There is no separate network listener, TLS identity, pairing format,
or outer application-authentication protocol. A fixed `ctl-ssh-v1` readiness
marker precedes the raw stream so startup output cannot be mistaken for an
`rmux-proto` frame.

## Connection command

When global `--host`/`-H` selects a remote target, every one-shot request or
interactive attachment starts the system OpenSSH client with the equivalent
fixed argument sequence:

```text
ssh -T \
  -o ClearAllForwardings=yes \
  -o ForwardAgent=no \
  -o ForwardX11=no \
  -o PermitLocalCommand=no \
  -o RemoteCommand=none \
  -- <destination> exec ctld connect
```

`<destination>` is an OpenSSH destination or `Host` alias. Host-key checking,
user authentication, certificates, agents, proxy jumps, ports, and connection
multiplexing remain OpenSSH configuration. `ctl` never disables host-key
verification, enables agent forwarding, creates a forwarding, or accepts a
user-controlled remote command.

Without `--host`, `ctl` connects directly to the current user's owner-only
local `rmuxd` endpoint and does not start SSH or `ctld`. The `rmux-proto`
request, attachment, detach, and reconnect behavior above that transport is
shared by both targets.

OpenSSH may reuse a healthy configured control master. A broken SSH transport
cannot resume an existing channel; `ctl` starts a replacement channel and
uses the `rmux-proto` attachment token described below.

## Remote command

`ctld connect` is a disposable, stateless process launched once per SSH
channel. It connects to the fixed per-user `rmuxd` data endpoint, starting only
an absolute-path companion `rmuxd` when configured and necessary, writes
`ctl-ssh-v1\n` to stdout, then copies raw bytes in both directions:

```text
SSH stdin  -> ctld connect -> rmuxd Unix socket
SSH stdout <- ctld connect <- rmuxd Unix socket
```

Completion or failure of either copy direction ends the relay and closes its
local socket. Diagnostics use stderr exclusively. The remote command accepts
no arbitrary local socket, forwarded address, or service name from the SSH
client. The sibling owner-only `rmuxd` maintenance endpoint is never exposed.

Non-interactive remote shell startup files must not write to stdout. Such bytes
precede the readiness marker, so `ctl` rejects the connection with a focused
startup-output error instead of feeding them to `rmux-proto`; stderr output is
safe.

## Authorization boundary

SSH authenticates the device and user. `ctld` adds no identity, authorization,
or capability registry. It runs with the SSH account's existing authority and
can reach only that account's fixed local `rmuxd` endpoint.

This does not grant a successfully authenticated SSH account new local
authority: the same account can already connect to its owner-only `rmuxd`
socket. Deployments that must grant `ctl` without ordinary SSH remote-command
access are outside the current design.

## Reconnect lifecycle

`rmuxd`, not `ctld`, owns reconnect state. A successful initial attachment
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
