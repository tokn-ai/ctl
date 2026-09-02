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

### Docker development target

The repository includes a development image that builds `ctld` and `rmuxd`
and exposes them through OpenSSH. It deliberately accepts only public-key
authentication and the fixed `exec ctld connect` remote command. Shells,
PTY allocation, forwarding, agent access, X11, tunnels, and SSH subsystems are
disabled.

Set an absolute path to the public-key file that may access the container, then
start the target:

```sh
export RMUX_AUTHORIZED_KEYS_FILE=/absolute/path/to/id_ed25519.pub
docker compose up --build --detach rmux-remote
```

The default host port is `2222`. Set `RMUX_SSH_PORT` before starting the
container to choose another port. Add a local OpenSSH alias so `ctl` can use
the port and matching private key through normal SSH configuration:

```sshconfig
Host rmux-docker
  HostName 127.0.0.1
  Port 2222
  User rmux
  IdentityFile /absolute/path/to/id_ed25519
  IdentitiesOnly yes
```

Inspect and trust the container host-key fingerprint before the first
connection:

```sh
docker compose exec rmux-remote \
  ssh-keygen -lf /etc/ssh/host_keys/ssh_host_ed25519_key.pub
```

Then exercise the real remote path:

```sh
ctl --host rmux-docker rmux list
ctl --host rmux-docker rmux new --name docker-test
ctl --host rmux-docker rmux attach docker-test
```

The desktop app uses the same OpenSSH transport. If the alias above already
exists, start `rmux-app`, choose **+ Host**, and activate the discovered
`rmux-docker` alias. Concrete aliases from `~/.ssh/config` and its `Include`
files are suggestions only; opening the picker does not contact them.

The app can also define the container without a pre-existing alias. In **+
Host**, enter `rmux@127.0.0.1:2222`, use a name such as `rmux-remote-test`, then
choose **Identity file** and enter the matching private-key path. These steps
open at the command palette location. Verify any SSH host-key prompt against
the fingerprint above. Once `ctld connect` succeeds, choose **OpenSSH config** to create a
reusable managed `Host` block, or **This app only** to keep those settings in
WebView local storage. The latter still invokes the system SSH client and does
not store the key contents. `ctld` is assumed to be on the remote `PATH`.
Password/passphrase and host-verification prompts use the same overlay on
macOS/Linux. Secrets are process-memory-only; after relaunch, click the host
chip to authenticate again if SSH config/agent alone is insufficient.

Local and container sessions then appear in one sidebar with host labels. New
shells default to local; the creation form can explicitly select the remote
host, while **New Tab in Current Folder** always inherits the active session's
host.

The `rmux_ssh_host_keys` volume preserves the SSH host identity across
container replacement. Terminal sessions remain memory-backed and disappear
when the container stops, matching the current `rmuxd` persistence model.

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
