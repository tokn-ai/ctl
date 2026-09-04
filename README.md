# ctl monorepo

[![CI](https://github.com/tokn-ai/ctl/actions/workflows/ci.yml/badge.svg)](https://github.com/tokn-ai/ctl/actions/workflows/ci.yml)

This repository will contain two independently useful products:

- `rmux`: persistent local terminal sessions;
- `ctl`: SSH-authorized access to remote `rmux` sessions.

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
cargo build -p ctl -p ctld -p taskd -p rmux -p rmuxd
```

The `rmux` and `rmuxd` binaries must be installed beside one another, or
`RMUXD_BIN` must name the daemon executable.

For local use, install both packages into your Cargo binary directory:

```sh
cargo install --path rmux/daemon
cargo install --path rmux/cli
```

For remote access, install `rmuxd` and `ctld` on the controlled device and
`ctl` on the client:

```sh
cargo install --path rmux/daemon
cargo install --path ctl/daemon
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
and progress/errors stay in the overlay. `ctld` is assumed on the remote `PATH`.
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
The target is local by default; no SSH process or `ctld` helper is involved:

```sh
ctl rmux list
ctl rmux attach development
```

Pass global `--host`/`-H` to redirect the same rmux command through SSH. The
value is an ordinary OpenSSH destination or `~/.ssh/config` host alias, and
the remote account must be able to run `ctld connect` non-interactively:

```sh
ctl --host workstation rmux list
ctl --host workstation rmux new --name development
ctl -H workstation rmux attach development
```

`ctld` has no network listener or application-level pairing state. It relays
the SSH channel to the same user's fixed local `rmuxd` endpoint. After an
unexpected SSH loss, `ctl` creates a replacement channel and `rmuxd` preserves
the logical attachment and its leases for 30 seconds by default. An explicit
`Ctrl-]` detach releases them immediately.

## Managed tasks

The initial task implementation manages local noninteractive commands. Task
definitions and completed run metadata persist in taskd; stdout and stderr are
kept in a bounded in-memory log for the current daemon lifetime.

```sh
ctl task create api --cwd ./service --start -- cargo run
ctl task list
ctl task show api
ctl task logs api
ctl task logs api --follow
ctl task stop api
ctl task restart api
ctl task remove api
```

On Unix, background tasks run in their own process group. Stop first sends a graceful
termination signal to the group and escalates if it does not exit. Interactive
rmux-backed tasks can be registered with `--mode interactive`, but starting
them is reserved for the next implementation stage. Remote task routing and
desktop workspace integration are also not implemented yet.

Windows background tasks use owner-restricted local named pipes and Job Objects.
Stop terminates the entire process tree; taskd exit also terminates its jobs.
Task completion follows the root process and cleans up remaining descendants.
`ctl task create` saves the caller's working directory unless `--cwd` is supplied.
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

Install `ctld.exe` and `rmuxd.exe` beside each other in a directory on the
remote user's PATH. Enable Windows OpenSSH Server with its default `cmd.exe`
shell, then select the server platform explicitly from either client platform:

```sh
ctl --host windows-host --remote-platform windows rmux new development -- cmd.exe /D /Q
ctl --host windows-host --remote-platform windows rmux attach development
```

The fixed Windows command is `ctld.exe connect`. The gateway relays only the
user's rmux data pipe, and starts the companion daemon when absent. The daemon
breaks away from the SSH job so its sessions survive disconnects. PowerShell
and custom SSH shells are not covered by this implementation. Remote task
routing and desktop remote-platform selection remain pending.
