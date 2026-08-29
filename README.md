# ctl monorepo

This repository will contain two independently useful products:

- `rmux`: persistent local terminal sessions;
- `ctl`: authenticated remote device control and access to remote `rmux`
  sessions.

The current milestone implements the first local `rmux` vertical slice on
macOS and other Unix platforms. Windows named-pipe IPC is not implemented yet.

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

Press `Ctrl-]` to detach without terminating the shell. End the session
explicitly with:

```sh
rmux kill work
```

The client starts a per-user `rmuxd` on demand. The daemon owns the PTY and
continues running after clients disconnect. It exits after its final session
ends. Raw output history is currently bounded and memory-backed. `rmuxd`
creates versioned terminal-state checkpoints so a new or reconnecting client
can restore the current screen without replaying an arbitrarily large journal.
Disk history, restart policies, cwd shell integration, and explicit input/layout
leases are later milestones.

Architecture and protocol details are in [`docs/architecture.md`](docs/architecture.md)
and [`docs/rmux-protocol.md`](docs/rmux-protocol.md).
