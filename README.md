# ctl monorepo

This repository will contain two independently useful products:

- `rmux`: persistent local terminal sessions;
- `ctl`: authenticated remote device control and access to remote `rmux`
  sessions.

The current MVP supports local `rmux` sessions, a local desktop terminal,
and authenticated `ctl` access to sessions on macOS and other Unix platforms.
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

The desktop app uses pnpm and Tauri 2. Build `rmuxd` into the shared Cargo
target directory before starting it so the app can auto-start its sibling
daemon:

```sh
cargo build -p rmuxd
cd rmux/gui
pnpm install
pnpm tauri dev
```

## Use

Create a detached persistent shell in the current directory:

```sh
rmux new --name work
```

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
ends. Raw output history is currently bounded and memory-backed. `rmuxd`
creates versioned terminal-state checkpoints so a new or reconnecting client
can restore the current screen without replaying an arbitrarily large journal.
Optional shell-awareness state is memory-only and separate from the raw output
journal. Disk-backed history and restart policies are later milestones.

The `rmux` desktop app lists and creates local sessions, renders one terminal
pane, and exposes input and layout ownership separately. Selecting a session
does not resize its PTY. **Use window for layout** is the explicit action that
acquires layout ownership and applies the window's measured terminal size.
Closing or detaching the app leaves the daemon-owned shell running.

Architecture and protocol details are in [`docs/architecture.md`](docs/architecture.md)
and [`docs/rmux-protocol.md`](docs/rmux-protocol.md). The remote setup is in
[`docs/remote-mvp.md`](docs/remote-mvp.md).
