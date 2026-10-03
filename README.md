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

The `ctl` binary bundles the [parent ctl skill](ctl/cli/skills/ctl/SKILL.md), focused
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

- [ctl-host](ctl/cli/skills/ctl-host/SKILL.md) (`ctl skill ctl-host`): saved hosts, methods, and connection status.
- [ctl-session](ctl/cli/skills/ctl-session/SKILL.md) (`ctl skill ctl-session`): persistent shells, sessions, and panes.
- [ctl-task](ctl/cli/skills/ctl-task/SKILL.md) (`ctl skill ctl-task`): managed tasks and reusable local definitions.
- [ctl-port](ctl/cli/skills/ctl-port/SKILL.md) (`ctl skill ctl-port`): daemon-owned local SSH forwards.
- [ctl-vpn](ctl/cli/skills/ctl-vpn/SKILL.md) (`ctl skill ctl-vpn`): local OpenConnect and Tailscale containers.

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
The desktop bundle identifier is `dev.tokn-ai.ctl.ctmux`, and its signed connection
helper uses `dev.tokn-ai.ctl.ctld`. Signing the helper requires a matching
provisioning profile.
Update clients, daemons, and remote agent bundles together. The renamed build
uses ctmux protocol 13, task protocol 4, task lifecycle protocol 2, ctld protocol
12, ctld lifecycle protocol 1, ctld one-shot helper API 1, remote identity
protocol 3, and remote maintenance protocol 2.

## Configuration and persistent state

Application configuration files and persistent component state use `~/.tokn/ctl`
on every platform, independently of the desktop bundle identifier:

| File or directory | Contents |
| --- | --- |
| `hosts.json` | Hosts shared by the CLI and desktop |
| `workspace.json` | Desktop sessions, tabs, and presentation state |
| `vpns.json` | Saved VPN profiles |
| `keybindings.json` | Desktop keyboard shortcuts |
| `tasks.json` | Global reusable task definitions |
| `taskd/` | Registered tasks and retained run metadata |
| `ctmux/desktop/sessions/` | Desktop session output cache and durable archives |
| `ctmux/desktop/archives/`, `ctmux/tui/archives/` | Client-specific text archives |

Project task definitions stay in `<project-root>/.ctl/tasks.json`. Explicit
`CTL_HOSTS_PATH`, `CTL_VPNS_PATH`, `CTL_TASKD_DATA_DIR`, and
`CTMUX_ARCHIVE_DIRECTORY` overrides retain their existing behavior. Old storage
locations are not read or migrated. SSH credentials remain in macOS Keychain;
runtime sockets continue to use private OS-specific runtime directories.
Remote identity and component bundles already use this root as `remote-id`,
`versions/`, and `current`.

## Build

Rust 1.97 or newer is required. The Rust packages use the MIT license.
See [Cargo publishing](docs/publishing.md) for package verification, installation,
and the dependency order for the first crates.io release.

After the packages and matching GitHub release are published, install the CLI
and its local terminal/task companions on macOS:

```sh
cargo install --locked ctl-cli ctmuxd ctl-taskd
ctl setup
```

`ctl setup` installs the signed and notarized `ctld.app` for the installed CLI's
version and Mac architecture. It downloads the matching `v<version>` release;
it does not select the latest release. The full bundle lives at
`~/.tokn/ctl/components/ctld/versions/<version>-<target>/ctld.app`. Setup updates
the release `current` symlink and a selection for its architecture and supported
APIs, replacing each link atomically. This is separate from the remote agent's
existing `~/.tokn/ctl/versions/` and `current` installation.

Setup checks the release checksum, Apple Developer ID signature, provisioning
profile, notarization, and helper build/API identity against the installation's
own manifest before selecting the helper. It needs no `sudo` and never starts,
stops, or restarts a daemon, so running connections continue using their current
daemon. `ctl setup --json`
prints the installed version, executable path, and whether the installation was
reused.

`CTLD_BIN` selects an explicit executable. Without that override, a standalone
macOS CLI prefers a verified, compatible managed `ctld.app`, then its own bundled
helper, then a nearby desktop bundle, sibling executable, or `PATH`. The desktop
continues to prefer its own bundled helper. On other Unix platforms, install
`ctld` from Cargo alongside the CLI; `ctl setup` is a macOS-only command. Source-built macOS
`ctld` remains useful for development but does not acquire our Apple signing
identity or Keychain entitlement through Cargo.

Official macOS CLI downloads embed the matching signed and notarized `ctld.app`.
They prepare that helper locally when a command needs to start a daemon and no
compatible shared app is selected; no helper download is needed. `ctl setup`
also uses the embedded bundle. Existing compatible daemon connections and passive
status queries do not trigger installation.
Build these CLI downloads with the dedicated [release command](docs/ci-bundles.md#build-a-cli-with-ctld-embedded).
Ordinary Cargo builds and `cargo install ctl-cli` also discover compatible shared
apps, including signed development installations; they contain no embedded helper.

Selections live at
`~/.tokn/ctl/components/ctld/selected/<target>-ctld12-lifecycle1-helper1`.
Compatibility requires the native architecture and the `ctld`, `ctld_lifecycle`,
and `ctld_helper` API versions. The helper's build identity must match its own
manifest; it does not have to match the CLI's commit, fingerprint, or release
version. Discovery verifies the selected app instead of choosing a cache entry
by its directory name or modification time. Unsafe or invalid selected
installations produce a verification error.

For a signed macOS CLI from a local checkout, provision once with the same
Xcode project used by Tauri:

```sh
node scripts/dev/ctl-signed.mts --provision
```

In Xcode, select the `ctld-provisioning` target, choose your team under
**Signing & Capabilities**, and build once. Then build and use the CLI:

```sh
node scripts/dev/ctl-signed.mts
target/ctl-dev/ctl --help
```

The build discovers your profile and matching Keychain certificate, refreshing
the profile through Xcode when needed. It compiles and signs `ctld.app`, embeds
it inside the signed CLI, and supports uncommitted source changes. No signing
environment variables or notarization credentials are required. The output
follows Cargo's configured target directory.

Development signing explicitly disables timestamps, so it does not depend on
Apple's timestamp service. Distributable releases still require secure timestamps.

The CLI prepares its helper when needed; `target/ctl-dev/ctl setup` also installs
it explicitly. Development helpers live under
`~/.tokn/ctl/components/ctld/development/<archive-sha256>/ctld.app`. They retain
their signature and provisioning checks, use a separate immutable cache, and
update the shared selection for their architecture and APIs while leaving the
release `current` symlink intact. All standalone CLI builds can reuse that
selection. Expired profiles require rebuilding. Existing compatible daemons keep
running until you explicitly restart them.

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
controlled device:

```sh
cargo install --path ctmux/daemon
cargo install --path task/daemon
cargo install --path ctl/agent
```

On a Linux client, install `ctl` and `ctld` together from the checkout:

```sh
cargo install --path ctl/daemon
cargo install --path ctl/cli
```

For the official signed macOS helper, use the published CLI installation and
`ctl setup` shown above. A source-built `ctld` is available for local development.

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

The `Desktop, control daemon, and remote-agent bundles` workflow builds static
Linux and native macOS remote bundles and desktop packages for x86-64 and ARM64. Main-branch
pushes and manual runs build both; `build_desktop=false` keeps a manual run
remote-only, as used by `pnpm agents:sync`. Each desktop package contains all
four remote targets and matching local `ctld`, `ctmuxd`, and `ctl-taskd` helpers.
Release bundle IDs are semantic versions; other runs include the source
revision so different development builds never share a remote install
directory. Tag names must match the Cargo and desktop versions as `v<version>`.

Successful full builds on main or a version tag create or refresh the
`v<version>` draft release with installers, macOS app archives, remote bundles,
manifests, and SHA-256 checksums. Version tags also include signed, notarized,
and stapled standalone `ctld.app` archives for both Mac architectures. Branch
builds remain Actions artifacts.
The workflow never publishes a release, preserves manually added assets and
notes, and leaves already-published versions unchanged. Bump the app version
to start the next draft after publishing.

Main development builds can produce unsigned macOS desktop packages when Apple
credentials are incomplete, and their draft notes identify them as unsigned.
Version-tag macOS builds require distribution signing and fail if the required
credentials are missing. Standalone `ctld` artifacts always require signing and
notarization; manual runs can request them with `build_ctld=true`. See
[release bundles and Apple signing](docs/ci-bundles.md) for the credential
requirements and release asset contract.

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
connection and automatically saves the named host to `~/.tokn/ctl/hosts.json`.
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
`~/.tokn/ctl/hosts.json` (`CTL_HOSTS_PATH` overrides it). Select hosts and methods
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

Interactive Unix connections offer to install matching components when the
remote agent is missing or uses the old `ctl-ssh-v2` protocol, then retry once.
Repair verifies any saved machine ID and the bundle's exact clean source
revision, reuses the chosen SSH route, and shows received bytes, speed, and
installation stages. It preserves running daemons and can be cancelled with
Ctrl-C. Piped commands and background reconnects never prompt. Local bundle
sets can be selected with `CTL_REMOTE_BUNDLES_DIR`; otherwise the CLI checks
its local resources, `~/.tokn/ctl/agent-bundles`, and the verified download cache
before the matching official release and existing exact-revision GitHub bundle
artifacts when `gh` is installed. Downloads are cached under
`~/.tokn/ctl/agent-bundles/<revision>/<target>/` for reuse across hosts; each reuse
checks the version, source revision, and checksum without contacting GitHub.
For source development, run `pnpm agents:sync` from `apps/desktop` at the same
clean pushed revision before rebuilding. See [remote setup](docs/remote-mvp.md).

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

Host settings are shared with the desktop in `~/.tokn/ctl/hosts.json`. CLI
overrides `CTL_HOSTS_PATH` and `CTL_VPNS_PATH` select alternate catalog files;
the default VPN file is `~/.tokn/ctl/vpns.json`. These commands do not edit SSH
config.

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
ctl vpn create
ctl vpn list
ctl vpn start NAME_OR_ID
ctl vpn stop NAME_OR_ID
```

`create` opens an interactive questionnaire for an OpenConnect or Tailscale
profile. It masks password input and saves the profile privately to
`~/.tokn/ctl/vpns.json`, shared with the desktop VPN page. Use `CTL_VPNS_PATH` to
select another catalog. Creation only saves settings; it does not start `ctld`,
build an image, or connect a VPN. Cancelling leaves the catalog unchanged.

`list` combines saved VPN profiles from `~/.tokn/ctl/vpns.json` with local
connections and compatible shared containers. It prints name, provider, state,
server or tailnet, username, SOCKS5 endpoint, and VPN ID. When any displayed VPN
is shared, a USE column distinguishes `owned` from `shared`; `owned` means the
selected daemon holds heartbeat interest. Profiles stay visible after their
containers are removed. The daemon probe is passive:
list never starts `ctld` or acquires heartbeat interest. A saved profile without
a runtime connection is disconnected only when inventory is complete; otherwise
its state is unavailable. Runtime connections without a saved profile also appear.

`start NAME_OR_ID` connects an existing saved profile, starting `ctld` when
needed. An exact stable `connection_id` takes precedence over a name; names must
match exactly and identify a single profile. Use the ID from `list` when names
are duplicated. Start reads saved credentials privately and reuses a compatible
container or recreates it when missing. To recover after container removal, run
`start` again and use the newly reported SOCKS5 endpoint. Omitting the selector
opens a profile picker in an interactive terminal; scripts must supply it.

Tailscale prints a browser sign-in link when needed. Starting the same saved
profile retains its device identity and login. The creation questionnaire asks
for an optional hostname and whether to accept advertised subnet routes.

Multiple VPNs can run independently. Stop accepts a saved profile's exact ID or
unique exact name, or a runtime VPN ID from list. With a current daemon, omitting
it opens a picker of local connections in an interactive terminal. Older daemons
support only one local connection, so an omitted selector uses their untargeted
stop directly. In scripts, an untargeted stop requires at most one local
connection. All five commands support `--json`.
Create remains interactive and returns only saved metadata, with prompts on
stderr. List JSON includes merged `entries`, raw runtime `connections`, capability
fields, and discovery warnings. If the saved catalog
cannot be read, runtime entries remain available with `profile_warnings`.
Saved metadata excludes passwords and URL credentials, paths, queries, and
fragments. Start launches `ctld` if needed. Stop leaves `ctld` running and
releases that daemon's heartbeat interest. A shared container remains available
while another daemon uses it, then exits after the final heartbeat expires. See the
[OpenConnect setup](docker/openconnect/README.md) for building the image and
creating a saved profile, or the [Tailscale guide](docker/tailscale/README.md)
for browser sign-in and persistent container state.

To delete a saved profile, run:

```sh
ctl vpn remove NAME_OR_ID
```

Remove selects an exact saved profile ID before a unique exact name. Omitting
the selector opens a saved-profile picker. It always requires interactive
confirmation, defaulting to No; there is no `--yes` bypass. Removal deletes the
saved catalog entry and retains Tailscale identity volumes. Cancelling leaves
the catalog unchanged without querying the daemon. After confirmation, removal
requires complete runtime inventory and a stopped VPN. Release its heartbeat
interests and wait for the container to exit first. Remove never starts or stops
a daemon or VPN; an unavailable or legacy inventory blocks deletion.

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

Task clients and daemons negotiate published contract `1.0.4`. Product release
versions are independent; compatible implementations can come from different
releases. See [protocol versioning](docs/protocol-versioning.md). SSH task routing additionally requires
the matching gateway and SSH forced-command allowlist; rebuilding the Docker
target updates all four binaries. Automatic restart policies remain pending.
The desktop task interface currently manages local tasks.

On Unix, background tasks run in their own process group. Stop first sends a
termination signal to the group and escalates if it does not exit.

Windows background tasks use owner-restricted local named pipes and Job Objects.
Stop terminates the entire process tree; ctl-taskd exit also terminates its jobs.
Task completion follows the root process and cleans up remaining descendants.
Task state defaults to `~/.tokn/ctl/taskd` on every platform. Unix directories
use owner-only permissions; Windows files inherit directory ACLs. Custom data
directories should be private to the user.

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
its per-connection lifetime. CLI and desktop service channels use the stable
`ctl-ssh-identity` marker, negotiate identity, then negotiate the selected service's
published contract. Update the
client, remote executables, and any SSH forced-command configuration together.

### Exited sessions

The desktop and TUI retain final terminal output after an exit or confirmed
missing-session response. Press a key to dismiss the ended pane or session;
transport outages continue to reconnect.

Dismissed and deleted sessions are stored on the client device until deleted.
The current screen and recent history appear first; older retained remote
history downloads in the background. Local history mirrors the remote bounded
window, and unavailable or incomplete history is marked visibly. Existing
archives remain readable.

Archives contain locally retained text; they remain available when the host is
offline, and do not revive a process. Open **Archived** in the desktop Sessions
sidebar, use **Ctrl+B A** in the TUI, or run `ctmux archives` followed by
`ctmux archive SESSION_ID`. Desktop and TUI maintain separate local archives.
Paged history uses published contract `1.1.14`; peers selecting `1.0.13`
continue to receive complete inline history.
Reading existing local archives does not require a connection or upgrade.
