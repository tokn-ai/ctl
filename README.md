# ctl monorepo

[![CI](https://github.com/tokn-ai/ctl/actions/workflows/ci.yml/badge.svg)](https://github.com/tokn-ai/ctl/actions/workflows/ci.yml)

This repository will contain two independently useful products:

- `ctmux`: persistent local terminal sessions;
- `ctl`: local and SSH-authorized access to terminal sessions and managed tasks.

The current MVP supports local `ctmux` sessions plus mixed local/SSH sessions
in both the desktop app and `ctl` on macOS and other Unix platforms. Windows
supports local ConPTY sessions through `ctmux` and `ctl ctmux`, plus background
tasks through `ctl task`. Windows `ctl --host HOST ctmux ...` also routes through
the system OpenSSH client. Unix hosts use the default remote command; Windows
hosts use `--remote-platform windows` and the default cmd.exe SSH shell.
Windows desktop support remains pending.

## Agent skills

The `ctl` binary bundles the [parent ctl skill](skills/ctl/SKILL.md), focused
child skills, and their supporting references. Read them without a source
checkout, daemon, configuration, or authentication:

```sh
ctl skill
ctl skill ctl-task
ctl skill task --file references/definitions.md
ctl skill --file references/setup.md
ctl skill --list
```

`ctl skill` prints the parent; selecting a name prints that skill's `SKILL.md`.
`--file PATH` selects a bundled Markdown file within the chosen skill, and
`--list` lists all bundled name/file pairs. Output preserves the document's
exact bytes. Short names `host`, `session`, `task`, `port`, and `vpn` are aliases
for their `ctl-` names. Read only the skill or reference needed for the task.

The parent covers workflow selection, one-off commands, file copies, and setup.
It routes to separate skills that can also be used independently:

- [ctl-host](skills/ctl-host/SKILL.md) (`ctl skill ctl-host`): saved hosts, methods, and connection status.
- [ctl-session](skills/ctl-session/SKILL.md) (`ctl skill ctl-session`): persistent shells, sessions, and panes.
- [ctl-task](skills/ctl-task/SKILL.md) (`ctl skill ctl-task`): managed tasks and reusable local definitions.
- [ctl-port](skills/ctl-port/SKILL.md) (`ctl skill ctl-port`): daemon-owned local SSH forwards.
- [ctl-vpn](skills/ctl-vpn/SKILL.md) (`ctl skill ctl-vpn`): local OpenConnect and Tailscale containers.

## Package names

The control packages use the `ctl-*` family and terminal packages use `ctmux-*`.
Cargo package names and installed command names are intentionally separate:

| Cargo package | Installed command |
| --- | --- |
| `ctl-cli` | `ctl` |
| `ctld` | `ctld` |
| `ctl-agent` | `ctl-agent` |
| `ctl-taskd` | `ctl-taskd` |
| `ctmux-cli` | `ctmux` |
| `ctmuxd` | `ctmuxd` |
| `ctmux-tui` | `ctmux-tui` |
| `ctmux-app` | desktop app `ctmux` |

For example, `cargo build -p ctl-cli -p ctmux-cli` builds `ctl` and `ctmux`.
The terminal subcommand is `ctl ctmux`; task-daemon maintenance remains
`ctl taskd restart`. Library crates use names such as `ctl-client`, `ctl-ipc`,
`ctl-task-client`, and `ctmux-client`.

This is a clean break from the development names. No old-name aliases or data
migration are provided. Default configuration, archives, runtime endpoints,
desktop identifiers, and Keychain services use the new names; existing data is
left untouched. Set `CTMUXD_BIN` / `CTMUX_RUNTIME_DIR` for terminal overrides and
`CTL_TASKD_BIN` / `CTL_TASKD_RUNTIME_DIR` / `CTL_TASKD_DATA_DIR` for task overrides.
The desktop bundle identifier is `io.ctmux.desktop`, and its signed connection
helper uses `io.ctmux.desktop.ctld`; signing requires profiles for these identifiers.
Update clients, daemons, and remote agent bundles together. The renamed build
uses ctmux protocol 13, task protocol 4, task lifecycle protocol 2, ctld protocol
12, remote identity protocol 3, and remote maintenance protocol 2.

## Build

```sh
cargo build --workspace
```

For the tmux-style terminal UI (local sessions, including when run inside SSH):

```sh
cargo build -p ctmux-cli -p ctmuxd
cargo run -p ctmux-cli
```

Use Ctrl+B then `?` for help, `%` to split right, and `d` to detach.
See [apps/tui](apps/tui/README.md) for controls and shared-view behavior.

For the Windows local CLI and daemon slice:

```sh
cargo build -p ctl-cli -p ctl-agent -p ctl-taskd -p ctmux-cli -p ctmuxd
```

The `ctmux` and `ctmuxd` binaries must be installed beside one another, or
`CTMUXD_BIN` must name the daemon executable.

For local use, install both packages into your Cargo binary directory:

```sh
cargo install --path ctmux/daemon
cargo install --path ctmux/cli
```

For remote access, install `ctmuxd`, `ctl-taskd`, and `ctl-agent` together on the
controlled device and `ctl` with `ctld` on each Unix client:

```sh
cargo install --path ctmux/daemon
cargo install --path task/daemon
cargo install --path ctl/agent
cargo install --path ctl/daemon
cargo install --path ctl/cli
```

The local task runner also requires `ctl-taskd` beside `ctl`, or `CTL_TASKD_BIN` set to
the daemon executable:

```sh
cargo install --path task/daemon
cargo install --path ctl/cli
```

The desktop app uses pnpm and Tauri 2. Build its local daemons into the shared
Cargo target directory before starting it so the app can auto-start the
sibling executables:

```sh
cargo build -p ctld -p ctmuxd -p ctl-taskd
cd apps/desktop
pnpm install
pnpm tauri dev
```

Tauri development starts even without remote install bundles and prints the
explicit sync command when they are absent or stale. To test first-time remote
installation, commit and push the current branch, then download or build the
bundle set for that exact commit:

```sh
pnpm agents:sync
```

`pnpm agents:sync --main` deliberately uses the latest successful `main`
bundle set when exact source parity is not required.

The `Desktop and remote-agent bundles` workflow builds static Linux and native
macOS remote bundles and desktop packages for x86-64 and ARM64. Main-branch
pushes and manual runs build both; `build_desktop=false` keeps a manual run
remote-only, as used by `pnpm agents:sync`. Each desktop package contains all
four remote targets and matching local `ctld`, `ctmuxd`, and `ctl-taskd` helpers.
Release bundle IDs are semantic versions; other runs include the source
revision so different development builds never share a remote install
directory. Tag names must match the app version as `v<version>`.

Successful full builds on main or a version tag create or refresh the
`v<version>` draft release with installers, macOS app archives, remote bundles,
manifests, and SHA-256 checksums. Branch builds remain Actions artifacts.
The workflow never publishes a release, preserves manually added assets and
notes, and leaves already-published versions unchanged. Bump the app version
to start the next draft after publishing.

When Apple signing credentials are incomplete, macOS packages still build
without signing or notarization and the draft notes identify them as unsigned.
These builds cannot store Touch ID-protected credentials. Configured signing
errors still fail the build rather than silently producing unsigned packages.

## Use

Open a new persistent session in the TUI, or create one detached for scripts:

```sh
ctmux
ctmux new -s work
ctmux new -As work       # attach if it exists, otherwise create
ctmux new -ds background # detached
```

The standalone `ctmux new` now attaches by default; existing scripts should add
`-d`. The optional `ctmux-tui` launcher remains available.

Without `--name`, `ctmuxd` assigns a short name such as `session-1`, increasing
monotonically for that daemon lifetime. Explicit names remain available for
scripts and stable workflows.

List and attach to sessions:

```sh
ctmux ls
ctmux attach -t work
```

A session is a listed root bound to a server-owned view. Each view owns one or
more terminals with independent PTYs and histories. Inspect its layout and use
the returned terminal IDs to split, promote, attach, or terminate a pane:

```sh
ctmux view work
ctmux split <terminal-id>            # side by side
ctmux split <terminal-id> --vertical # stacked
ctmux attach <terminal-id>
ctmux promote <terminal-id> --name scratch
ctmux merge scratch work
ctmux kill-terminal <terminal-id>
```

Splitting inherits the source terminal's known cwd unless `--cwd` is supplied.
Promotion and merging preserve terminal IDs, processes, history, and existing
attachments. `ctmux list` shows roots only; `ctmux kill work` terminates every
terminal in that root. These commands also work through `ctl ctmux`.

The desktop renders the server layout with **Split right**, **Split below**,
**Move to new session**, and **Terminate pane** controls. Its merge selector
combines remembered sessions on the same host. Protocol version 10 requires
updating both the client and daemon; existing version 9 daemons are not migrated
while running.

On macOS and Linux, `ctmuxd` observes the managed shell's physical cwd and
foreground job on a background worker, including while detached. The reusable
[`ctmux-process-info`](ctmux-process-info/README.md) crate reads OS process metadata without
shell hooks, arguments, environment, or dotfile changes. Unavailable information
stays unknown; a process name is not a command line or prompt-state report.

Optionally enable richer shell awareness in an interactive shell startup file.
The snippet is inert outside an ctmux-managed session; automatic integration
without startup-file edits is a later step:

```sh
# ~/.zshrc
eval "$(ctmux shell init zsh)"
```

`zsh` reports its cwd, prompt phase, and live editable command buffer. `bash`
reports its shell identity, cwd, and prompt phase; it deliberately does not
claim a reliable live edit buffer on Bash 3.2. Shell-reported cwd takes precedence
over OS observations, preserving logical paths through symlinks. Inspect the
non-sensitive state of a session with:

```sh
ctmux state work
```

`ctmux state` never prints an editable command buffer. The protocol lets an
input-owning GUI request it explicitly, but the initial desktop client
deliberately leaves it redacted.
That redaction applies to shell metadata; normal terminal echo remains part of
the raw terminal stream seen by attached viewers.

Press `Ctrl+B`, then `d` to detach without terminating the shell. Use
`Ctrl+B`, then `?` for TUI shortcuts. End the session explicitly with:

```sh
ctmux kill-session -t work
```

The first normal attachment claims an unheld input lease, so another normal
attachment becomes view-only instead of stealing keystrokes. Request a viewer
explicitly with:

```sh
ctmux attach work --read-only
```

The TUI requests the shared view resize lease and fits the canvas when available.
Use uppercase `I` and `R` after the prefix to take or release input and resize
ownership. Read-only attachment requests neither lease.

For the original single-terminal presenter, use `--raw`. It detaches with
Ctrl+] and resizes only with an explicit `--resize` request:

```sh
ctmux attach --raw work --resize
```

The client starts a per-user `ctmuxd` on demand. The daemon owns the PTY and
continues running after clients disconnect. It exits after its final session
ends. Raw output replay and normalized logical history are bounded and
memory-backed. `ctmuxd` creates paired versioned history/live checkpoints so a
new or reconnecting GUI can reconstruct scrollback and the current screen
without replaying an arbitrarily large journal.
Optional shell-awareness state is memory-only and separate from the raw output
journal. Disk-backed history and restart policies are later milestones.

The `ctmux` desktop app restores a disk-backed workspace of known local and
remote sessions, automatically reconnecting the selected local tab on startup.
Remote hosts stay disconnected until explicitly opened; **Connect host** resumes
that host's selected tab, or its first open tab. **Add existing session**
explicitly discovers one host's inventory and remembers only chosen entries;
opening a session connects on demand. A host is a named machine, independent
of its IP address, hostname, or gateway route. Each host supports one remote
account/environment and multiple named connection methods. **Add host** asks
for the SSH address, a display name, and authentication, then verifies the
connection and automatically saves the named host to `~/.tokn/ctmux/hosts.json`.
New addresses start with a method named `SSH`; saved aliases retain their `SSH config` method.
Additional methods and gateway routes are
available in **Host settings**. New-host creation does not modify OpenSSH config.

Concrete aliases from `~/.ssh/config` and its `Include` files are available in
**Connect host** and session pickers. They appear in the sidebar after connecting;
aliases with remembered work remain visible after restart. Connecting to an
alias does not import its definition. Saving a
customization in **Host settings** creates a saved host with the same identity.
Aliases already represented by a saved method are hidden unless their projected
identity still has workspace references. **Host settings** lets you rename the
machine or methods, add or edit methods, choose the preferred method, and
explicitly **Connect using** another method. New direct methods can optionally
export a managed OpenSSH config entry with **Also save to OpenSSH config**.
**Connect host**, **New shell**, and **Add existing session** start connecting immediately
when the host has one method. Hosts with several methods show a picker with the
preferred method selected by default. Choosing another method connects through
it without changing the preference; **Connect using** starts its named method
directly. Failures never select another method automatically.
Saving settings leaves existing session transports unchanged
until an explicit connection. Session, tab, and port references retain the same
host ID through these changes. Workspace schema 8 moves existing saved hosts and
gateways into `hosts.json`, preserving their IDs and a backup of the workspace.

Online devices from the installed Tailscale client appear separately as **Tailscale · Virtual**
in Add host, Connect host, New shell, and Add existing session. Discovery runs in
the background and leaves definitions in memory until customized or explicitly
saved. Unused devices stay out of the main sidebar. Device Online/Offline is a
Tailscale observation; the host's Connected status still means authenticated SSH.
Opening Add host refreshes discovery, hiding stale device suggestions until it
finishes; Refresh sessions also refreshes discovery. Saved hosts and remembered
work remain available when devices go offline. Saved Tailscale methods retain
a stable device binding across name/address changes and preserve the host's account
identity. Connections use the existing SSH authentication flow.

The client can be installed as a macOS app without adding it to `PATH`: ctmux checks
`/Applications/Tailscale.app` and `~/Applications/Tailscale.app`, as well as CLI
locations, and forces CLI mode when invoking the app executable. Discovery does
not install, sign in to, or reconfigure Tailscale.

The app records the host's remote environment ID and installed agent/bundle version.
The remote stores its ID in `~/.tokn/ctl/remote-id`; installed bundles live under
`~/.tokn/ctl/versions`, with `~/.tokn/ctl/current` selecting the active bundle.
Every method on a host must reach that same environment. Hosts are never merged
automatically because their remote IDs match. Hover over a host heading to see
its last observed version and remote ID.
An older agent offers **Update remote components** before identity discovery.
Authentication uses the command-palette overlay, which also handles destructive
close/restart confirmations and **New Shell** input.
**New Shell** asks for a host (Local first) and an optional working directory;
blank uses that host's home directory. Escape cancels before creation starts,
and progress/errors stay in the overlay. If `ctl-agent` is absent, the app can
install its bundled, checksummed remote components for that user and retry.
Installation shows the current archive or component, a transfer progress bar,
bytes received by the host, and recent transfer speed. Uploads have no overall
time limit while bytes continue advancing; a speed-aware stall watchdog replaces
the old three-minute installation deadline. Authentication prompts pause that
watchdog, and Escape cancels the installation.
Each row and tab carries its host; create, attach, reconnect, and kill
operations use that host's selected connection method without changing session
identity.
It renders one terminal pane and exposes input and layout ownership separately.
Selecting a session does not resize its PTY. **Resize with window** explicitly
acquires layout ownership and continuously matches the PTY to the window;
turning it off releases layout ownership. A session created in the GUI starts
with this mode enabled because that window establishes its initial layout.
GUI-created shells receive a daemon-assigned name. **Disconnect** closes the
active tab and detaches its view while leaving the daemon-owned shell running;
**Terminate session** explicitly terminates the session for every attached client. Closing
the app itself detaches its active view and does not terminate any sessions.
**Remove from workspace** forgets an entry without killing its shell. See
[workspace persistence](docs/ctmux-workspace.md) for disk storage and migration.

The desktop command palette opens with `Cmd-Shift-P` on macOS and
`Ctrl-Shift-P` on Windows/Linux. The same command registry supplies app-local
shortcuts for creating and switching sessions; only exact registered
combinations are intercepted, so ordinary terminal keystrokes continue to the
PTY.

`Cmd-T` on macOS or `Ctrl-Shift-T` on Windows/Linux opens a tab in the existing
WebView with a new persistent shell in the current observed shell directory.
Only the active tab is attached through the window's attachment actor. Closing
a tab detaches its view without terminating the daemon-owned session. The
command is available only when shell awareness has a reported or OS-observed cwd.

`Cmd-W` detaches the active tab, while `Cmd-E` opens the existing confirmation
for terminating its daemon-owned session. The Windows/Linux equivalents are
`Ctrl-Shift-W` and `Ctrl-Shift-E`, leaving ordinary terminal control keys
untouched. On macOS these are native application-menu accelerators; `Cmd-Q`
retains its standard meaning and quits the app without terminating sessions.

## Local and remote control

`ctmux` remains the canonical local session CLI:

```sh
ctmux list
ctmux new -ds development
ctmux attach -t development
```

`ctl ctmux` shares command parsing through ctl's selected target, retaining
detached creation and the single-terminal presenter for local and SSH transports.
The target is local by default; no SSH process or `ctl-agent` helper is involved:

```sh
ctl ctmux list
ctl ctmux attach development
```

Pass global `--host`/`-H` to redirect the same ctmux command through SSH. The
value first selects a saved ctl host by name or ID, then falls back to an
ordinary OpenSSH destination or `~/.ssh/config` host alias. Unix
clients ask the per-user `ctld` to establish or reuse an authenticated OpenSSH
control master, then add the app-managed per-user installation to the fixed
remote command's `PATH` before falling back to the remote account's ordinary
non-interactive `PATH`:

```sh
ctl --host workstation ctmux list
ctl --host workstation ctmux new --name development
ctl -H workstation ctmux attach development
```

`ctl-agent` has no network listener or application-level pairing state. It relays
the SSH channel to the same user's fixed local `ctmuxd` endpoint. After an
unexpected SSH loss, `ctl` creates a replacement channel and `ctmuxd` preserves
the logical attachment and its leases for 30 seconds by default. An explicit
`Ctrl-]` detach releases them immediately.

### Saved hosts and connection status

```sh
ctl host add work 10.0.0.20 --user alice
ctl host list
ctl host show work --json
ctl host status work
ctl host update work --name office --port 2222
ctl host update office --clear port

ctl host method add office VPN 10.0.0.20 --user alice --vpn company
ctl host method prefer office VPN
ctl host method update office VPN --destination 10.0.0.21
ctl host method remove office SSH

ctl host connect office
ctl host status office --method VPN
ctl host disconnect office
ctl host remove office
```

`host` manages the same saved definitions as the desktop in
`~/.tokn/ctmux/hosts.json` (`CTL_HOSTS_PATH` overrides it). Select hosts and methods
by name or stable ID. These management commands require saved hosts; to save an
SSH config alias, use `ctl host add work my-ssh-alias --ssh-config`. `host list`
shows saved definitions only, not unsaved SSH config or Tailscale discoveries.

`list`, `show`, and `status` observe existing ctld connections without starting
a daemon, connecting, or starting a VPN. Status is per method: `connected`
means an existing SSH control endpoint responds; `disconnected` means no
connection is observed; `paused` means an explicit disconnect; `unknown` includes
the observation error. This is not a remote reachability check. `show` also
includes any previously learned remote identity/version. All three accept
`--json`. On Windows, catalog management is supported and connection status is
reported as `unsupported`; connect/disconnect currently require Unix.

`connect` authenticates the preferred method (or `--method NAME_OR_ID`) and
starts its saved VPN if needed. It opens no shell and installs no remote
component. `disconnect` pauses all saved methods, or just `--method`, using
ctld's existing disconnect policy; active channels may close. It does not stop
the VPN itself. `remove` only deletes the saved definition: remote sessions,
active connections, credentials, and workspace references are retained.

Updates preserve host/method IDs and any pinned remote identity. Omitted
settings stay unchanged; `--clear` accepts a comma-separated list of optional
settings (see `ctl host update --help`). Use `--gateway ID` repeatedly to set an
ordered route through existing saved gateways, or `--vpn ID` to select a saved
VPN. Use `host method` to manage alternate routes. The preferred method cannot
be removed until another is selected. Catalog edits use the desktop's lock,
atomic writes, and content revisions; a concurrent edit fails without
replacing it. Reload an already-open desktop to see CLI changes.

### Shells, commands, and file copies

The native shell command creates a new persistent ctmux session by default.
A named session attaches if it already exists, or is created if absent:

```sh
ctl shell
ctl -H work shell
ctl -H work shell --session development
ctl -H work shell --plain
ctl -H work exec -- uname -a
ctl -H work exec -- sh -c 'printf "%s\n" "$HOME"'
```

`--plain` opens an ordinary shell. `exec` runs once without allocating a PTY,
streams stdin/stdout/stderr, and returns the command's exit status. Unix exec
arguments are quoted individually; explicitly invoke `sh -c` for shell syntax.
The default target is local. Persistent remote shells require `ctl-agent` and
`ctmuxd` on the destination, as with `ctl ctmux`; plain SSH operations need only
the remote SSH service.

`ssh` and `scp` accept the system OpenSSH command syntax. A destination such as
`work` selects the same saved ctl host used by `-H work`:

```sh
ctl ssh work
ctl ssh work 'uname -a'
ctl scp ./file.txt work:/tmp/
ctl scp work:/tmp/file.txt ./
ctl --method vpn ssh work
ctl -H work --method vpn shell --session development
```

Saved hosts use their preferred connection method unless `--method` selects a
method name or ID. Duplicate names are rejected; use a stable host ID instead.
`user@work` overrides the saved account. Saved names take precedence over
matching SSH-config aliases. Unknown destinations retain normal OpenSSH lookup.
Ctl-specific flags go before `ssh`/`scp`; everything after those subcommands
belongs to OpenSSH, including the remote command's flags.

On Unix, ordinary saved-host SSH/SCP sessions reuse the exact ctld master,
including VPN/SOCKS routes. The saved VPN is started when needed; Tailscale
device bindings resolve their current address. Explicit connection/configuration
options such as `-i`, `-p`, `-F`, `-J`, `-S`, or transport-related `-o` options
use OpenSSH directly with saved defaults instead of silently borrowing an
incompatible master. Explicit proxy options override the saved route.
`ssh -G` resolves settings without opening SSH or starting a VPN. SCP uses ctl
as its SSH subprocess, retaining OpenSSH's paths, progress display, and transfer
protocol; an explicit `scp -S` selects the user's own transport instead.

Host settings are shared with the desktop in `~/.tokn/ctmux/hosts.json`. CLI
overrides `CTL_HOSTS_PATH` and `CTL_VPNS_PATH` select alternate catalog files;
the default VPN file is the desktop's `io.ctmux.desktop/vpns.json` under the
platform configuration directory. These commands do not edit SSH config.

### Managed port forwards

```sh
ctl -H work port add 8080:127.0.0.1:80 --id web
ctl -H work port list
ctl -H work port remove web
```

`port add` connects as needed and returns after ctld starts the listener.
Forwards outlive the CLI process and remain in ctld's runtime registry until
removed or the daemon exits; they are not saved across daemon restarts. Bind
addresses are loopback-only, defaulting to `127.0.0.1`. Bracket IPv6 addresses,
for example `[::1]:8080:[2001:db8::2]:80`. Add/list support `--json`.
List/remove select the chosen connection method; use the same `--method` when
managing a forward created on a nonpreferred route. The desktop Ports refresh
also discovers these runtime forwards and can stop them without making them
persistent workspace entries.

## Managed VPN

Manage local OpenConnect and Tailscale containers through `ctld` using the `ctl` CLI:

```sh
ctl vpn start --env-file .env
ctl vpn start-tailscale --id my-tailnet
ctl vpn status
ctl vpn stop VPN_ID
```

Tailscale prints a browser sign-in link when needed. Reuse the same `--id` to
retain its device identity and login. Add `--hostname NAME` to name the device or
`--accept-routes` to use advertised subnet routes.

Status prints a table of VPN IDs, providers, states, servers, usernames, and randomly
allocated loopback SOCKS5 endpoints. Multiple VPNs can run independently. Use the
ID from the table to stop one; an untargeted stop requires at most one active VPN.
Add `--json` for scripts. Start launches `ctld`
if needed. Stop leaves `ctld` running and releases that daemon's heartbeat
interest. A shared container remains available while another daemon uses it,
then exits after the final heartbeat expires. See the
[OpenConnect setup](docker/openconnect/README.md) for building the image and
configuring the private env file, or the [Tailscale guide](docker/tailscale/README.md)
for browser sign-in and persistent container state. The desktop VPN page manages
both providers through saved JSON profiles.

## Managed tasks

Tasks support local and SSH background commands and interactive terminals on
Unix and Windows. Registered-task definitions and the latest run metadata persist in ctl-taskd.
Background stdout and stderr use a bounded in-memory log; interactive input and
output stay in ctmuxd.

Reusable local definitions are shared by the CLI and desktop, separately from
registered tasks. Save to the current project or explicitly use the global catalog:

```sh
ctl task save build -- cargo build
ctl task definitions list
ctl task definitions show build
ctl task create app-build --from-definition build --start
ctl task save shell --global --mode interactive -- bash
```

The desktop Tasks sidebar selects Global or a project directory. Saving checks
the original definition revision; concurrent edits keep your draft and report a
conflict. See [shared task definitions](docs/task-definitions.md) for paths,
scope selection, updating definitions, and workspace migration.

```sh
ctl task create api --cwd ./service --start -- cargo run
ctl task list
ctl task show api
ctl task start api
ctl task logs api
ctl task logs api --follow
ctl task stop api
ctl task restart api
ctl task remove api
```

Interactive tasks use `--mode interactive` and attach through ctmux:

```sh
ctl task create shell --mode interactive --start -- bash
ctl task attach shell
ctl task stop shell
ctl task restart shell
```

Use `cmd.exe /D /Q` instead of `bash` for a Windows command shell. Taskd manages
the run; ctmuxd owns its process and PTY (ConPTY on Windows). `ctl task show`
also reports the session ID for `ctl ctmux attach`. `ctl task logs` applies only
to background tasks.

Global `--host` selects the same remote target for task registration, lifecycle,
logs, and interactive attachment:

```sh
ctl --host workstation task create api --cwd /home/me/service --start -- cargo run
ctl --host workstation task list
ctl --host workstation task logs api --follow
ctl --host workstation task create shell --mode interactive --start -- bash
ctl --host workstation task attach shell
ctl --host workstation task stop shell
ctl --host workstation task remove shell
```

Task requests use the same fixed managed-directory `PATH` prefix and
`ctl-agent connect --service task` command as ctmux connections.
Interactive attachment opens a separate ctmux channel to that same host; remote
socket paths in task metadata are never opened on the client. A local create
defaults to the caller's working directory. A remote create defaults to the
remote user's home; `--cwd` names a remote path, with relative paths resolved
against that home.

Interactive runs survive ctl-taskd restart and are reconciled with the same ctmuxd
instance. Ctmuxd retains exit results until ctl-taskd records them. Replacing or
losing ctmuxd fails the affected runs; ctl-taskd does not automatically recreate
them. Starting and restarting remain explicit operations.

Build and install `ctl`, `ctl-taskd`, and `ctmuxd` together. Task protocol version 4
requires matching clients and daemons. SSH task routing additionally requires
the matching gateway and SSH forced-command allowlist; rebuilding the Docker
target updates all four binaries. Automatic restart policies remain pending.
The desktop task interface currently manages local tasks.

On Unix, background tasks run in their own process group. Stop first sends a
termination signal to the group and escalates if it does not exit.

Windows background tasks use owner-restricted local named pipes and Job Objects.
Stop terminates the entire process tree; ctl-taskd exit also terminates its jobs.
Task completion follows the root process and cleans up remaining descendants.
Windows state defaults to `%LOCALAPPDATA%\ctl-taskd` and inherits filesystem ACLs;
custom data directories should be private to the user.

Architecture and protocol details are in [`docs/architecture.md`](docs/architecture.md)
and [`docs/ctmux-protocol.md`](docs/ctmux-protocol.md). Numbered
[`design proposals`](docs/proposals/README.md) record feature intent and major
ownership boundaries. The remote setup is in
[`docs/remote-mvp.md`](docs/remote-mvp.md).
The [Windows CI exploration](docs/windows-ci.md) records verified compilation
boundaries and native tests for background tasks and ConPTY sessions. The
Windows desktop backend and native shell metadata remain separate work.

### Windows SSH hosts

Install `ctl-agent.exe`, `ctl-taskd.exe`, and `ctmuxd.exe` together in a directory on the
remote user's PATH. Enable Windows OpenSSH Server with its default `cmd.exe`
shell, then select the server platform explicitly from either client platform:

```sh
ctl --host windows-host --remote-platform windows ctmux new --name development -- cmd.exe /D /Q
ctl --host windows-host --remote-platform windows ctmux attach development
ctl --host windows-host --remote-platform windows task list
```

The fixed Windows commands are `ctl-agent.exe connect` for ctmux and
`ctl-agent.exe connect --service task` for tasks. The gateway relays the selected
user-owned data pipe and starts its companion daemon when absent. The daemon
breaks away from the SSH job so its sessions or tasks survive disconnects.
PowerShell and custom SSH shells are not covered by this implementation.
Desktop remote-platform selection remains pending.

The SSH gateway is named `ctl-agent` (`ctl-agent.exe` on Windows), reflecting
its per-connection lifetime. Ordinary connections use the `ctl-ssh-v1` transport
marker, then negotiate the selected service's protocol version. Update the
client, remote executables, and any SSH forced-command configuration together.

### Exited sessions

The desktop and TUI retain final terminal output after an exit or confirmed
missing-session response. Press a key to dismiss the ended pane or session;
transport outages continue to reconnect.

Dismissed and deleted sessions are stored on the client device for seven days.
Archives contain locally retained text; they remain available when the host is
offline, and do not revive a process. Open **Archived** in the desktop Sessions
sidebar, use **Ctrl+B A** in the TUI, or run `ctmux archives` followed by
`ctmux archive SESSION_ID`. Desktop and TUI maintain separate local archives.
No archive protocol or daemon upgrade is required.
