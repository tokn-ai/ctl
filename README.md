# ctl monorepo

This repository will contain two independently useful products:

- `rmux`: persistent local terminal sessions;
- `ctl`: SSH-authorized access to remote `rmux` sessions.

The current MVP supports local `rmux` sessions, a local desktop terminal,
and SSH-backed `ctl` access to sessions on macOS and other Unix platforms.
Windows local IPC is not implemented yet.

## Build

```sh
cargo build --workspace
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

Optionally enable shell awareness in an interactive shell startup file. The
snippet is inert outside an rmux-managed session:

```sh
# ~/.zshrc
eval "$(rmux shell init zsh)"
```

`zsh` reports its cwd, prompt phase, and live editable command buffer. `bash`
reports its shell identity, cwd, and prompt phase; it deliberately does not
claim a reliable live edit buffer on Bash 3.2. Inspect the non-sensitive state
of a session with:

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

The `rmux` desktop app lists and creates local sessions, renders one terminal
pane, and exposes input and layout ownership separately. Selecting a session
does not resize its PTY. **Resize with window** explicitly acquires layout
ownership and continuously matches the PTY to the window; turning it off
releases layout ownership. A session created in the GUI starts with this mode
enabled because that window establishes its initial layout.
GUI-created shells receive a daemon-assigned name. **Disconnect** closes the
active tab and detaches its view while leaving the daemon-owned shell running;
**Close** explicitly terminates the session for every attached client. Closing
the app itself detaches its active view and does not terminate any sessions.

The desktop command palette opens with `Cmd-Shift-P` on macOS and
`Ctrl-Shift-P` on Windows/Linux. The same command registry supplies app-local
shortcuts for creating and switching sessions; only exact registered
combinations are intercepted, so ordinary terminal keystrokes continue to the
PTY.

`Cmd-T` on macOS or `Ctrl-Shift-T` on Windows/Linux opens a tab in the existing
WebView with a new persistent shell in the current shell-reported directory.
Only the active tab is attached through the window's attachment actor. Closing
a tab detaches its view without terminating the daemon-owned session. The
command is available only when shell awareness has reported a cwd.

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

Architecture and protocol details are in [`docs/architecture.md`](docs/architecture.md)
and [`docs/rmux-protocol.md`](docs/rmux-protocol.md). The remote setup is in
[`docs/remote-mvp.md`](docs/remote-mvp.md).
