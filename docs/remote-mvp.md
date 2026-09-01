# Remote rmux over SSH

This milestone lets an SSH-authorized user access persistent `rmux` sessions
without exposing a separate network service. OpenSSH provides reachability,
host verification, encryption, and user authentication.

## On the controlled device

Build or install `rmuxd` and `ctld` for the same OS user. They should be
available in the non-interactive SSH command environment. `ctld` starts a
sibling `rmuxd` on demand when both binaries are installed together; `rmuxd`
may instead be started independently.

Verify that the fixed remote command works and that non-interactive startup
files produce no stdout:

```text
ssh -T <host> exec ctld connect
```

The command waits for `rmux-proto` input, so terminate this manual probe after
confirming it starts without diagnostics.

## On the client device

`rmux` defines the canonical command surface. `ctl rmux` redirects those same
commands through ctl's selected target. Select an ordinary OpenSSH destination
or `~/.ssh/config` alias with global `--host`/`-H`:

```text
ctl --host <host> rmux list
ctl --host <host> rmux attach <session>
```

Useful session commands:

```text
ctl --host <host> rmux new --name <session>
ctl --host <host> rmux state <session>
ctl --host <host> rmux kill <session>
```

An ordinary attachment requests input but does not resize the remote PTY. Use
`ctl --host <host> rmux attach <session> --read-only` for a viewer, or add
`--resize` only when deliberately claiming layout ownership. Press `Ctrl-]`
to detach and release the attachment immediately without terminating the
shell.

After an unexpected SSH interruption, `ctl` reconnects with exponential
backoff. OpenSSH may reuse a configured control master; otherwise it creates a
new SSH connection. `rmuxd` preserves the logical attachment and both leases
for 30 seconds by default, while output resumes from the last renderer-applied
raw sequence.

## Operational limits

- SSH authentication must work for a non-interactive remote command. Password
  and host-key prompts remain OpenSSH behavior, but key, agent, or certificate
  authentication is preferable for unattended reconnects.
- `ctld` is currently Unix-only because its fixed local endpoint is a Unix
  socket. A future Windows endpoint can retain the same SSH transport.
- Journals, checkpoints, shell awareness, and reconnect tokens are memory-only.
- Generic commands, files, jobs, port forwarding, desktop streaming, and
  `rmuxd` maintenance control are not exposed by `ctld`.
