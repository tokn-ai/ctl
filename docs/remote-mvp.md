# Remote rmux MVP

This milestone lets a paired client attach to a named persistent shell on a
Unix device. Tailscale supplies reachability; `ctl` still verifies the device
certificate and a client Ed25519 signature itself.

## On the controlled device

Build or install `rmuxd`, `ctld`, and `rmux` for the same OS user. Initialize
the device identity once:

```text
ctld init
```

Start the gateway on the device's concrete Tailscale IP and a chosen port:

```text
ctld serve --listen <tailscale-ip>:9944
```

The gateway uses the per-user local `rmuxd` endpoint. If `rmuxd` is not yet
running, `ctld` starts only an absolute-path sibling `rmuxd` binary; otherwise
start `rmuxd` yourself or pass `ctld serve --rmuxd-bin <absolute-path>`.

Create a short-lived invitation for the client device:

```text
ctld pair create --endpoint <device-name-or-tailscale-ip>:9944 --label <client-name>
```

That command prints a one-time bearer secret. Transfer it through a channel
you trust. Do not place it in source code, logs, screenshots, or a shell
command line.

## On the client device

Run `ctl pair --alias <device-alias>` and paste the invitation when prompted.
It reads one line from standard input, so the token does not need to enter
shell history. A piped single invitation line also works.

Confirm pairing and create/attach to the default named shell:

```text
ctl hosts
ctl shell <device-alias>
```

`ctl shell` creates `shell` only if it does not exist. Specify a different
name with `ctl shell <device-alias> <session>`. It reconnects after a transient
network or gateway interruption using the last received raw output sequence;
it never replays keyboard input. If a former attachment is still silent but
not yet expired, the reconnect temporarily remains view-only, then retries
only its originally requested unheld input/layout leases. It never forces a
takeover from an active attachment.

Useful session commands:

```text
ctl session list <device-alias>
ctl session new <device-alias> --name <session>
ctl session kill <device-alias> <session>
```

An ordinary attach requests input but does not resize the remote PTY. Use
`ctl shell <device-alias> <session> --read-only` for a viewer, or add `--resize`
only when you deliberately want to acquire layout ownership and resize the
PTY. Press Ctrl-] to detach without terminating the shell.

## Limits of this milestone

The session journal and checkpoints are memory-backed. Optional shell awareness
is also memory-only: the remote rmux tunnel preserves its versioned snapshots,
but the current raw `ctl shell` presenter does not render metadata or request
editable command text. Disk-backed history, restartable task generations,
Windows local transport, and other remote-administration services are
deliberately deferred. See `docs/architecture.md` and `docs/ctl-protocol.md`
for the ownership and security boundaries.
