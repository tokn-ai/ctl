# ctl monorepo

[![CI](https://github.com/tokn-ai/ctl/actions/workflows/ci.yml/badge.svg)](https://github.com/tokn-ai/ctl/actions/workflows/ci.yml)

This repository will contain two independently useful products:

- `rmux`: persistent local terminal sessions;
- `ctl`: local and SSH-authorized access to terminal sessions and managed tasks.

The current MVP supports local `rmux` sessions plus mixed local/SSH sessions
in both the desktop app and `ctl` on macOS and other Unix platforms. Windows
supports local ConPTY sessions through `rmux` and `ctl rmux`, plus background
tasks through `ctl task`. Windows `ctl --host HOST rmux ...` also routes through
the system OpenSSH client. Unix hosts use the default remote command; Windows
hosts use `--remote-platform windows` and the default cmd.exe SSH shell.
Windows desktop support remains pending.

## Build

```sh
cargo build --workspace
```

For the Windows local CLI and daemon slice:

```sh
cargo build -p ctl -p ctl-agent -p taskd -p rmux -p rmuxd
```

The `rmux` and `rmuxd` binaries must be installed beside one another, or
`RMUXD_BIN` must name the daemon executable.

For local use, install both packages into your Cargo binary directory:

```sh
cargo install --path rmux/daemon
cargo install --path rmux/cli
```

For remote access, install `rmuxd`, `taskd`, and `ctl-agent` together on the
controlled device and `ctl` on the client:

```sh
cargo install --path rmux/daemon
cargo install --path task/daemon
cargo install --path ctl/agent
cargo install --path ctl/cli
```

The local task runner also requires `taskd` beside `ctl`, or `TASKD_BIN` set to
the daemon executable:

```sh
cargo install --path task/daemon
cargo install --path ctl/cli
```

The desktop app uses pnpm and Tauri 2. Build `rmuxd` into the shared Cargo
target directory before starting it so the app can auto-start its sibling
daemon:

```sh
cargo build -p rmuxd
cd apps/rmux
pnpm install
pnpm tauri dev
```

## Use

Create a detached persistent shell in the current directory:

```sh
rmux new
rmux new --name work
```

Without `--name`, `rmuxd` assigns a short name such as `session-1`, increasing
monotonically for that daemon lifetime. Explicit names remain available for
scripts and stable workflows.

List and attach to sessions:

```sh
rmux list
rmux attach work
```

On macOS and Linux, `rmuxd` observes the managed shell's physical cwd and
foreground job on a background worker, including while detached. The reusable
[`process-info`](process-info/README.md) crate reads OS process metadata without
shell hooks, arguments, environment, or dotfile changes. Unavailable information
stays unknown; a process name is not a command line or prompt-state report.

Optionally enable richer shell awareness in an interactive shell startup file.
The snippet is inert outside an rmux-managed session; automatic integration
without startup-file edits is a later step:

```sh
# ~/.zshrc
eval "$(rmux shell init zsh)"
```

`zsh` reports its cwd, prompt phase, and live editable command buffer. `bash`
reports its shell identity, cwd, and prompt phase; it deliberately does not
claim a reliable live edit buffer on Bash 3.2. Shell-reported cwd takes precedence
over OS observations, preserving logical paths through symlinks. Inspect the
non-sensitive state of a session with:

```sh
rmux state work
```

`rmux state` never prints an editable command buffer. The protocol lets an
input-owning GUI request it explicitly, but the initial desktop client
deliberately leaves it redacted.
That redaction applies to shell metadata; normal terminal echo remains part of
the raw terminal stream seen by attached viewers.

Press `Ctrl-]` to detach without terminating the shell. End the session
explicitly with:

```sh
rmux kill work
```

The first normal attachment claims an unheld input lease, so another normal
attachment becomes view-only instead of stealing keystrokes. Request a viewer
explicitly with:

```sh
rmux attach work --read-only
```

Attaching never changes the existing PTY size. To deliberately claim layout
ownership and resize it once to the current terminal, use:

```sh
rmux attach work --resize
```

The client starts a per-user `rmuxd` on demand. The daemon owns the PTY and
continues running after clients disconnect. It exits after its final session
ends. Raw output replay and normalized logical history are bounded and
memory-backed. `rmuxd` creates paired versioned history/live checkpoints so a
new or reconnecting GUI can reconstruct scrollback and the current screen
without replaying an arbitrarily large journal.
Optional shell-awareness state is memory-only and separate from the raw output
journal. Disk-backed history and restart policies are later milestones.

The `rmux` desktop app restores a disk-backed workspace of known local and
remote sessions, automatically reconnecting the selected local tab on startup.
Remote hosts stay disconnected until explicitly opened; **Connect host** resumes
that host's selected tab, or its first open tab. **Add existing session**
explicitly discovers one host's inventory and remembers only chosen entries;
opening a session connects on demand. Its **+ Host** picker discovers concrete
aliases from the user's OpenSSH config without connecting to them; selecting
one makes it an active target. A new hostname can be saved as a reusable,
managed OpenSSH `Host` block or as structured app-local connection settings.
Add Host uses the command-palette overlay for host, name, authentication, and
storage prompts, with connection verification before saving. The same overlay
handles destructive close/restart confirmations and **New Shell** input.
**New Shell** asks for a host (Local first) and an optional working directory;
blank uses that host's home directory. Escape cancels before creation starts,
and progress/errors stay in the overlay. `ctl-agent` is assumed on the remote `PATH`.
Each row and tab carries its host; create, attach, reconnect, and kill
operations always use that session's original target.
It renders one terminal pane and exposes input and layout ownership separately.
Selecting a session does not resize its PTY. **Resize with window** explicitly
acquires layout ownership and continuously matches the PTY to the window;
turning it off releases layout ownership. A session created in the GUI starts
with this mode enabled because that window establishes its initial layout.
GUI-created shells receive a daemon-assigned name. **Disconnect** closes the
active tab and detaches its view while leaving the daemon-owned shell running;
**Close** explicitly terminates the session for every attached client. Closing
the app itself detaches its active view and does not terminate any sessions.
**Remove from workspace** forgets an entry without killing its shell. See
[workspace persistence](docs/rmux-workspace.md) for disk storage and migration.

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

`rmux` remains the canonical local session CLI:

```sh
rmux list
rmux new --name development
rmux attach development
```

`ctl rmux` reuses that exact command surface through ctl's selected target.
The target is local by default; no SSH process or `ctl-agent` helper is involved:

```sh
ctl rmux list
ctl rmux attach development
```

Pass global `--host`/`-H` to redirect the same rmux command through SSH. The
value is an ordinary OpenSSH destination or `~/.ssh/config` host alias, and
the remote account must be able to run `ctl-agent connect` non-interactively:

```sh
ctl --host workstation rmux list
ctl --host workstation rmux new --name development
ctl -H workstation rmux attach development
```

`ctl-agent` has no network listener or application-level pairing state. It relays
the SSH channel to the same user's fixed local `rmuxd` endpoint. After an
unexpected SSH loss, `ctl` creates a replacement channel and `rmuxd` preserves
the logical attachment and its leases for 30 seconds by default. An explicit
`Ctrl-]` detach releases them immediately.

## Managed tasks

Tasks support local and SSH background commands and interactive terminals on
Unix and Windows. Task definitions and the latest run metadata persist in taskd.
Background stdout and stderr use a bounded in-memory log; interactive input and
output stay in rmuxd.

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

Interactive tasks use `--mode interactive` and attach through rmux:

```sh
ctl task create shell --mode interactive --start -- bash
ctl task attach shell
ctl task stop shell
ctl task restart shell
```

Use `cmd.exe /D /Q` instead of `bash` for a Windows command shell. Taskd manages
the run; rmuxd owns its process and PTY (ConPTY on Windows). `ctl task show`
also reports the session ID for `ctl rmux attach`. `ctl task logs` applies only
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

Task requests use the fixed `exec ctl-agent connect --service task` command.
Interactive attachment opens a separate rmux channel to that same host; remote
socket paths in task metadata are never opened on the client. A local create
defaults to the caller's working directory. A remote create defaults to the
remote user's home; `--cwd` names a remote path, with relative paths resolved
against that home.

Interactive runs survive taskd restart and are reconciled with the same rmuxd
instance. Rmuxd retains exit results until taskd records them. Replacing or
losing rmuxd fails the affected runs; taskd does not automatically recreate
them. Starting and restarting remain explicit operations.

Build and install `ctl`, `taskd`, and `rmuxd` together. This task protocol is
version 3; restart an older taskd before using it. Restart an older rmuxd to
enable managed sessions (this terminates its existing terminals). Existing
background task records remain readable. SSH task routing additionally requires
the updated gateway and any SSH forced-command allowlist; rebuilding the Docker
target updates all four binaries. Automatic restart policies remain pending.
The desktop task interface currently manages local tasks.

On Unix, background tasks run in their own process group. Stop first sends a
termination signal to the group and escalates if it does not exit.

Windows background tasks use owner-restricted local named pipes and Job Objects.
Stop terminates the entire process tree; taskd exit also terminates its jobs.
Task completion follows the root process and cleans up remaining descendants.
Windows state defaults to `%LOCALAPPDATA%\ctl\taskd` and inherits filesystem ACLs;
custom data directories should be private to the user.

Architecture and protocol details are in [`docs/architecture.md`](docs/architecture.md)
and [`docs/rmux-protocol.md`](docs/rmux-protocol.md). Numbered
[`design proposals`](docs/proposals/README.md) record feature intent and major
ownership boundaries. The remote setup is in
[`docs/remote-mvp.md`](docs/remote-mvp.md).
The [Windows CI exploration](docs/windows-ci.md) records verified compilation
boundaries and native tests for background tasks and ConPTY sessions. The
Windows desktop backend and native shell metadata remain separate work.

### Windows SSH hosts

Install `ctl-agent.exe`, `taskd.exe`, and `rmuxd.exe` together in a directory on the
remote user's PATH. Enable Windows OpenSSH Server with its default `cmd.exe`
shell, then select the server platform explicitly from either client platform:

```sh
ctl --host windows-host --remote-platform windows rmux new development -- cmd.exe /D /Q
ctl --host windows-host --remote-platform windows rmux attach development
ctl --host windows-host --remote-platform windows task list
```

The fixed Windows commands are `ctl-agent.exe connect` for rmux and
`ctl-agent.exe connect --service task` for tasks. The gateway relays the selected
user-owned data pipe and starts its companion daemon when absent. The daemon
breaks away from the SSH job so its sessions or tasks survive disconnects.
PowerShell and custom SSH shells are not covered by this implementation.
Desktop remote-platform selection remains pending.

The SSH gateway is named `ctl-agent` (`ctl-agent.exe` on Windows), reflecting
its per-connection lifetime. When upgrading from the former gateway name,
update the client, remote executable, and any SSH forced-command configuration
together. The `ctl-ssh-v1` transport marker and rmux wire protocol are unchanged.
