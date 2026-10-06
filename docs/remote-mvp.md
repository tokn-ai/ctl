# Remote terminals and tasks over SSH

An SSH-authorized user can access persistent `ctmux` sessions and managed tasks
without exposing a separate network service. OpenSSH provides reachability,
host verification, encryption, and user authentication.

## On the controlled device

Build or install `ctmuxd`, `ctl-taskd`, `ctl-agent`, and `ctld` for the same OS user. They may
be installed together in the app-managed data directory or made available in
the non-interactive SSH command environment. `ctl-agent` starts a
sibling daemon on demand when the binaries are installed together; either daemon
may instead be started independently. Task control selects `ctl-taskd` explicitly,
and interactive tasks also require `ctmuxd` with managed-session support.

Verify that the fixed remote command works. Clients tolerate bounded startup
stdout before readiness, but shell startup diagnostics should use stderr:

```text
ssh -T <host> exec ctl-agent connect
ssh -T <host> exec ctl-agent connect --service task
exec ctl-agent connect --identity
exec ctl-agent connect --service task --identity
```

For a Windows host using the default cmd.exe SSH shell, the corresponding
probes use `ctl-agent.exe connect` or `ctl-agent.exe connect --service task`. Use
`ctl --host <host> --remote-platform windows ctmux ...` for normal commands.

Each command waits for its service's protocol input, so terminate the manual
probe after confirming it starts without diagnostics.

### Docker development target

The repository includes a development image that builds `ctl-agent`, `ctl`,
`ctmuxd`, and `ctl-taskd` and exposes the daemons through OpenSSH. It accepts only
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
export CTMUX_AUTHORIZED_KEYS_FILE=/absolute/path/to/id_ed25519.pub
docker compose up --build --detach ctmux-remote
```

The default host port is `2222`. Set `CTMUX_SSH_PORT` before starting the
container to choose another port. Add a local OpenSSH alias so `ctl` can use
the port and matching private key through normal SSH configuration:

```sshconfig
Host ctmux-docker
  HostName 127.0.0.1
  Port 2222
  User ctmux
  IdentityFile /absolute/path/to/id_ed25519
  IdentitiesOnly yes
```

Inspect and trust the container host-key fingerprint before the first
connection:

```sh
docker compose exec ctmux-remote \
  ssh-keygen -lf /etc/ssh/host_keys/ssh_host_ed25519_key.pub
```

Then exercise the real remote path:

```sh
ctl --host ctmux-docker ctmux list
ctl --host ctmux-docker ctmux new --name docker-test
ctl --host ctmux-docker ctmux attach docker-test
ctl --host ctmux-docker task create hello --start -- sh -c 'printf "hello from ctl-taskd\n"'
ctl --host ctmux-docker task logs hello
```

The desktop app uses the same OpenSSH transport. If the alias above already
exists, start `ctmux-app`, choose **+ Host**, and activate the discovered
`ctmux-docker` alias. Concrete aliases from `~/.ssh/config` and its `Include`
files are suggestions only; opening the picker does not contact them.

The app can also define the container without a pre-existing alias. In **+
Host**, enter `ctmux@127.0.0.1:2222`, use a name such as `ctmux-remote-test`, then
choose **Identity file** and enter the matching private-key path. These steps
open at the command palette location. Verify any SSH host-key prompt against
the fingerprint above. Once `ctl-agent connect --identity` succeeds, choose **OpenSSH config** to create a
reusable managed `Host` block, or **This app only** to keep those settings in
the app's native workspace file. The latter still invokes the system SSH client and does
not store the key contents. An app with a synchronized bundle set can install a
missing `ctl-agent`, `ctmuxd`, `ctl-taskd`, and `ctld` bundle under the remote user's
`~/.tokn/ctl/versions` directory and retry. `~/.tokn/ctl/current` selects the active
bundle, and `~/.tokn/ctl/remote-id` stores the stable environment ID. Release
bundles use the app version as their immutable
install ID; development bundles include their Git revision so one source build
cannot silently reuse another build's binaries.
The installation overlay reports platform detection, checksum verification,
the archive upload, extraction, component checks, and activation. Transfer
percentages and speed use byte counts confirmed by the remote host. Healthy
uploads can take longer than three minutes: a stall is detected only after no
new bytes arrive for a speed-dependent interval of 30 seconds to five minutes.
Platform detection and the upload connection each allow one minute of inactivity;
extraction allows two minutes, and other installation stages allow 30 seconds.
Time spent answering authentication prompts does not consume these intervals.
Password/passphrase and host-verification prompts originate in the per-user
`ctld` broker on macOS/Linux and use the initiating client's prompt UI. After
its OpenSSH control master authenticates on macOS, a newly entered reusable
secret gets an explicit Yes, No, or Never save choice before any remote
environment identity comparison. This also works when remote components are
not installed because authentication belongs to the master rather than the
`ctl-agent` channel. Yes stores the secret device-locally in Keychain under
user-presence protection, allowing Touch ID or the macOS account password.
A successful connection can reuse authorization for a fixed 24-hour window,
ending on lock/sleep or explicit disconnect; reconnects do not extend it.
Never suppresses future save
offers for that destination without retaining the secret. Only `ctld` accesses
Keychain, and Linux discards newly entered reusable secrets after authentication.

The desktop discovers a stable ctl environment ID and installed version during
connection verification. A different address with the same ID automatically recovers
the saved host and tabs; older agents offer a component update first. The Docker
fixture persists this ID in its `ctl_data` volume across container replacement.
The fixed-command allowlist includes the identity flag without accepting arbitrary
remote commands. See `docs/ctmux-workspace.md` for identity storage and migration.

An interactive CLI connection to a Unix host with missing remote components or
an old `ctl-ssh-v2` agent offers to install matching components and retry once.
The CLI verifies a saved machine ID using the old agent's identity response
before uploading; if that ID cannot be verified, repair stops. It reuses the
selected authenticated SSH connection, including its VPN/gateway route. Piped
commands and later reconnects never prompt or install automatically.

Repair verifies the complete build and uses the explicit upload selection in
`~/.tokn/ctl/components`. The selection is shared across remote hosts of that
target and stays stable until Sync/Update changes it. If there is no selection,
repair can initialize one from a verified schema-2 local resource bundle, an
existing legacy cache, the matching official release, or an existing GitHub
bundle artifact for the client revision. It never starts a workflow. Development
`pnpm agents:sync` explicitly imports and selects all four CI targets.

Each bundle contains `ctl-agent`, `ctmuxd`, `ctl-taskd`, and `ctld`. Import verifies
archive and binary checksums, a common identified build, and agreement between
the outer and archived advertisements. Client and companion contracts must have
explicit shared published versions. The selected bundle may differ from the
client's product version or source revision. CI artifact lookup still needs a
clean, identified client revision; release reuse depends on advertised contracts.
Legacy schema-1 artifacts cannot become a complete managed selection.

Publication and selection use private staging and atomic rename. Corrupt or
incompatible selections stop repair instead of changing the build automatically.
Use `ctl components sync --from <directory> --target <target> --purpose upload`
to select a verified replacement. `CTL_REMOTE_BUNDLES_DIR` supplies the initial
candidate when no upload selection exists; it does not override a selection.
See [Components and updates](component-updates.md) for layout and local use.

Upload progress shows the archive, received bytes, speed, and installation
stage; a healthy transfer has no overall time limit. Ctrl-C cancels repair.

Installation selects `~/.tokn/ctl/current` without restarting running daemons or
replacing a different existing bundle with the same ID. Existing old `rmux`
sessions can keep running separately. If a running service still has an
incompatible protocol after installation, the retried connection reports that
error; inspect its sessions before considering a disruptive restart.

The app restores known sessions from disk and automatically attaches the last
selected tab if it is local. Remote hosts stay disconnected on startup.
**Connect host** authenticates and resumes that host's selected saved tab, or
its first open tab if another host was selected. Use **Add existing session** to
discover and remember sessions already running in the container; simply adding
a host does not import its daemon's inventory. Opening a session connects to
its host on demand. See `docs/ctmux-workspace.md` for persistence and migration.

Known local and container sessions appear in one sidebar with host labels. New
shells default to local; **New Shell** uses the command-palette overlay to
choose a host and working directory. Only submission contacts that host;
authentication can be established with **Connect host** first. **New Tab in
Current Folder** always inherits the active session's host.

The image also includes `ctl` for local debugging against the same services:

```sh
docker compose exec --user ctmux ctmux-remote ctl task list
docker compose exec --user ctmux ctmux-remote ctl ctmux list
```

The gateway starts `ctl-taskd` on demand, sharing `/run/ctmux` with `ctmuxd` for
interactive sessions. `CTL_TASKD_RUNTIME_DIR=/run/ctl-taskd` and
`CTL_TASKD_DATA_DIR=/var/lib/ctl-taskd` select its private endpoint and metadata. Both
directories are owned by the `ctmux` account (UID 1000) with mode `0700`.

The `ctmux_ssh_host_keys` volume preserves the SSH host identity across container
replacement. The `ctl_taskd_data` volume preserves task definitions and active/latest
run metadata. Background logs and terminal journals remain in memory. Stopping
the container ends its processes and terminals; saved task definitions remain,
and interrupted runs are reconciled as failed without automatic restart.
Task working directories and generated files need their own bind mounts or
volumes if they should survive replacement.

### Upgrading the Docker target

After updating the checkout, use the same Compose project name, public-key file,
and SSH port as the existing target:

```sh
docker compose up --build --detach ctmux-remote
```

This rebuilds and replaces a changed container. Finish or stop active sessions
and tasks before replacement; their processes cannot be migrated into the new
container. Keep the named volumes and avoid `docker compose down --volumes` when
retaining SSH identity and task definitions. If ctl-taskd was previously run in a
custom image without the metadata volume, stop it and copy its data directory
into the new volume with UID 1000 ownership before starting the replacement.

An error mentioning `only 'exec ctld connect' is permitted` identifies a container
from before the gateway rename. Update its image and forced-command script
together. An older ctmuxd can still serve ordinary protocol-9 terminals but must
also be upgraded to support interactive tasks; renaming the gateway alone does
not add that daemon capability.

## On the client device

`ctmux` defines the canonical command surface. `ctl ctmux` redirects those same
commands through ctl's selected target. Select an ordinary OpenSSH destination
or `~/.ssh/config` alias with global `--host`/`-H`:

```text
ctl --host <host> ctmux list
ctl --host <host> ctmux attach <session>
```

Useful session commands:

```text
ctl --host <host> ctmux new --name <session>
ctl --host <host> ctmux state <session>
ctl --host <host> ctmux kill <session>
```

An ordinary attachment requests input but does not resize the remote PTY. Use
`ctl --host <host> ctmux attach <session> --read-only` for a viewer, or add
`--resize` only when deliberately claiming layout ownership. Press `Ctrl-]`
to detach and release the attachment immediately without terminating the
shell.

After an unexpected SSH interruption, `ctl` reconnects with exponential
backoff. On Unix clients, it reopens channels through the private master managed
by `ctld`; an expired master requires authentication through `ctld` before another
channel can open. Desktop methods can enable **Use SSH-config master** to honor
configured connection sharing, with a private master fallback when sharing is
unconfigured; SSH-config aliases enable this by default. `ctmuxd` preserves the logical attachment and both leases
for 30 seconds by default, while output resumes from the last renderer-applied
raw sequence.

## Managed tasks on the selected host

Use the same `--host` on every operation. Names and IDs are scoped to that host's
ctl-taskd; local tasks and tasks on another host are separate inventories.

```sh
ctl --host ctmux-docker task create worker --cwd /home/ctmux -- sh -c 'printf "ready\n"'
ctl --host ctmux-docker task list
ctl --host ctmux-docker task show worker
ctl --host ctmux-docker task start worker
ctl --host ctmux-docker task logs worker --follow
ctl --host ctmux-docker task restart worker
ctl --host ctmux-docker task stop worker
ctl --host ctmux-docker task remove worker
ctl --host ctmux-docker task create shell --mode interactive --start -- sh
ctl --host ctmux-docker task attach shell
```

When `--cwd` is omitted, a remote task starts in the remote user's home directory.
An explicit `--cwd` is a remote path; relative paths resolve against that home,
not the client's checkout. The selected host must contain the command, files,
and dependencies the task uses.

`task attach` resolves the task through the task gateway, then attaches through
the ordinary ctmux gateway on the same SSH target. It never opens a remote socket
path on the client. Interactive input, geometry, output, and reconnect retain
ctmux behavior; background logs use the task protocol. The desktop task UI
currently manages local tasks, while remote tasks are available through the CLI.

## Operational limits

- SSH authentication must work for a non-interactive remote command. Password
  and host-key prompts remain OpenSSH behavior, but key, agent, or certificate
  authentication is preferable for unattended reconnects.
- Windows hosts use `--remote-platform windows` with the server's default
  cmd.exe shell. Install `ctl-agent.exe`, `ctl-taskd.exe`, and `ctmuxd.exe` together
  on the remote PATH. Their data endpoints are owner-restricted named pipes. PowerShell/custom
  server shells and desktop remote-platform selection remain unverified or
  unimplemented.
- Journals, checkpoints, shell awareness, and reconnect tokens are memory-only.
- Arbitrary gateway commands, files, port forwarding, desktop streaming, and
  `ctmuxd` maintenance control are not exposed by `ctl-agent`.
