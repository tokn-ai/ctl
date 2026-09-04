# Remote terminals and tasks over SSH

An SSH-authorized user can access persistent `rmux` sessions and managed tasks
without exposing a separate network service. OpenSSH provides reachability,
host verification, encryption, and user authentication.

## On the controlled device

Build or install `rmuxd`, `taskd`, and `ctl-agent` for the same OS user. They should
be available in the non-interactive SSH command environment. `ctl-agent` starts a
sibling daemon on demand when the binaries are installed together; either daemon
may instead be started independently. Task control selects `taskd` explicitly,
and interactive tasks also require `rmuxd` with managed-session support.

Verify that the fixed remote command works and that non-interactive startup
files produce no stdout:

```text
ssh -T <host> exec ctl-agent connect
ssh -T <host> exec ctl-agent connect --service task
```

For a Windows host using the default cmd.exe SSH shell, the corresponding
probes use `ctl-agent.exe connect` or `ctl-agent.exe connect --service task`. Use
`ctl --host <host> --remote-platform windows rmux ...` for normal commands.

Each command waits for its service's protocol input, so terminate the manual
probe after confirming it starts without diagnostics.

### Docker development target

The repository includes a development image that builds `ctl-agent`, `ctl`,
`rmuxd`, and `taskd` and exposes the daemons through OpenSSH. It accepts only
public-key authentication and these exact remote commands:

```text
exec ctl-agent connect
exec ctl-agent connect --service task
```

The allowlist maps each command to a literal executable and argument list. Shells,
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
ctl --host rmux-docker task create hello --start -- sh -c 'printf "hello from taskd\n"'
ctl --host rmux-docker task logs hello
```

The desktop app uses the same OpenSSH transport. If the alias above already
exists, start `rmux-app`, choose **+ Host**, and activate the discovered
`rmux-docker` alias. Concrete aliases from `~/.ssh/config` and its `Include`
files are suggestions only; opening the picker does not contact them.

The app can also define the container without a pre-existing alias. In **+
Host**, enter `rmux@127.0.0.1:2222`, use a name such as `rmux-remote-test`, then
choose **Identity file** and enter the matching private-key path. These steps
open at the command palette location. Verify any SSH host-key prompt against
the fingerprint above. Once `ctl-agent connect` succeeds, choose **OpenSSH config** to create a
reusable managed `Host` block, or **This app only** to keep those settings in
the app's native workspace file. The latter still invokes the system SSH client and does
not store the key contents. `ctl-agent` is assumed to be on the remote `PATH`.
Password/passphrase and host-verification prompts use the same overlay on
macOS/Linux. Secrets are process-memory-only; after relaunch, click the host
chip to authenticate again if SSH config/agent alone is insufficient.

The app restores known sessions from disk and automatically attaches the last
selected tab if it is local. Remote hosts stay disconnected on startup.
**Connect host** authenticates and resumes that host's selected saved tab, or
its first open tab if another host was selected. Use **Add existing session** to
discover and remember sessions already running in the container; simply adding
a host does not import its daemon's inventory. Opening a session connects to
its host on demand. See `docs/rmux-workspace.md` for persistence and migration.

Known local and container sessions appear in one sidebar with host labels. New
shells default to local; **New Shell** uses the command-palette overlay to
choose a host and working directory. Only submission contacts that host;
authentication can be established with **Connect host** first. **New Tab in
Current Folder** always inherits the active session's host.

The image also includes `ctl` for local debugging against the same services:

```sh
docker compose exec --user rmux rmux-remote ctl task list
docker compose exec --user rmux rmux-remote ctl rmux list
```

The gateway starts `taskd` on demand, sharing `/run/rmux` with `rmuxd` for
interactive sessions. `TASKD_RUNTIME_DIR=/run/taskd` and
`TASKD_DATA_DIR=/var/lib/taskd` select its private endpoint and metadata. Both
directories are owned by the `rmux` account (UID 1000) with mode `0700`.

The `rmux_ssh_host_keys` volume preserves the SSH host identity across container
replacement. The `taskd_data` volume preserves task definitions and active/latest
run metadata. Background logs and terminal journals remain in memory. Stopping
the container ends its processes and terminals; saved task definitions remain,
and interrupted runs are reconciled as failed without automatic restart.
Task working directories and generated files need their own bind mounts or
volumes if they should survive replacement.

### Upgrading the Docker target

After updating the checkout, use the same Compose project name, public-key file,
and SSH port as the existing target:

```sh
docker compose up --build --detach rmux-remote
```

This rebuilds and replaces a changed container. Finish or stop active sessions
and tasks before replacement; their processes cannot be migrated into the new
container. Keep the named volumes and avoid `docker compose down --volumes` when
retaining SSH identity and task definitions. If taskd was previously run in a
custom image without the metadata volume, stop it and copy its data directory
into the new volume with UID 1000 ownership before starting the replacement.

An error mentioning `only 'exec ctld connect' is permitted` identifies a container
from before the gateway rename. Update its image and forced-command script
together. An older rmuxd can still serve ordinary protocol-9 terminals but must
also be upgraded to support interactive tasks; renaming the gateway alone does
not add that daemon capability.

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

## Managed tasks on the selected host

Use the same `--host` on every operation. Names and IDs are scoped to that host's
taskd; local tasks and tasks on another host are separate inventories.

```sh
ctl --host rmux-docker task create worker --cwd /home/rmux -- sh -c 'printf "ready\n"'
ctl --host rmux-docker task list
ctl --host rmux-docker task show worker
ctl --host rmux-docker task start worker
ctl --host rmux-docker task logs worker --follow
ctl --host rmux-docker task restart worker
ctl --host rmux-docker task stop worker
ctl --host rmux-docker task remove worker
ctl --host rmux-docker task create shell --mode interactive --start -- sh
ctl --host rmux-docker task attach shell
```

When `--cwd` is omitted, a remote task starts in the remote user's home directory.
An explicit `--cwd` is a remote path; relative paths resolve against that home,
not the client's checkout. The selected host must contain the command, files,
and dependencies the task uses.

`task attach` resolves the task through the task gateway, then attaches through
the ordinary rmux gateway on the same SSH target. It never opens a remote socket
path on the client. Interactive input, geometry, output, and reconnect retain
rmux behavior; background logs use the task protocol. The desktop task UI
currently manages local tasks, while remote tasks are available through the CLI.

## Operational limits

- SSH authentication must work for a non-interactive remote command. Password
  and host-key prompts remain OpenSSH behavior, but key, agent, or certificate
  authentication is preferable for unattended reconnects.
- Windows hosts use `--remote-platform windows` with the server's default
  cmd.exe shell. Install `ctl-agent.exe`, `taskd.exe`, and `rmuxd.exe` together
  on the remote PATH. Their data endpoints are owner-restricted named pipes. PowerShell/custom
  server shells and desktop remote-platform selection remain unverified or
  unimplemented.
- Journals, checkpoints, shell awareness, and reconnect tokens are memory-only.
- Arbitrary gateway commands, files, port forwarding, desktop streaming, and
  `rmuxd` maintenance control are not exposed by `ctl-agent`.
