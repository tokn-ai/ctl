---
name: ctl-session
description: "Create, attach to, inspect, split, merge, or terminate persistent local or SSH terminal sessions with ctl shell and ctl ctmux. Use when work needs a persistent PTY; managed task lifecycle and background logs belong to ctl-task."
---

# ctl Sessions

Use `ctl shell` for an interactive persistent shell, and `ctl ctmux` for session
and terminal operations. Omit `-H` for local work; use `ctl -H HOST [--method
NAME_OR_ID] ...` for a saved host, SSH alias, or destination. Keep the same
target and selected method throughout the workflow. Local sessions require
`ctmuxd`; remote sessions require `ctl-agent` and `ctmuxd` for the SSH account,
with `ctld` on Unix clients.

## Create and inspect

```sh
ctl ctmux list
ctl ctmux new --detached --name development --cwd /absolute/project
ctl -H work ctmux new --detached --name build --cwd /srv/app -- cargo build
ctl -H work ctmux list
ctl -H work ctmux view build
ctl -H work ctmux state build
```

Inspect inventory before reusing or terminating a session; use returned IDs
when names are ambiguous. `ctl ctmux new` returns a session ID and name without
attaching in the current implementation; spell `--detached` explicitly in
automation. Standalone `ctmux new` attaches by default.

Omitting the program uses the target's default shell. Commands are argv, so use
an explicit shell for shell expressions. Omitted local cwd uses the caller's
current directory; omitted remote cwd uses the remote home. Supply a verified
absolute path when the directory matters: explicit relative paths are passed
to the daemon, and an invalid terminal cwd can fall back to the target's home.
`--cwd` affects creation, not a session that already exists.

`list` reports root sessions. `view` returns a JSON layout with terminal IDs;
`state` prints advisory shell/cwd/prompt metadata without disclosing typed or
running command text. The displayed cwd can abbreviate home as `~`. Do not
invent `--json` for list/state or a `send-keys` command.

## Attach and detach

```sh
ctl -H work shell --session development
ctl -H work shell --resume
ctl -H work ctmux --resume
ctl -H work ctmux attach development
ctl -H work ctmux attach development --read-only
ctl -H work ctmux attach development --resize
```

`ctl shell` requires terminal stdin and stdout. Without `--session` or `--resume`
it creates a new session; a supplied name attaches if present or creates if
absent. `--resume` opens a host-scoped manager without attaching or creating
when sessions exist; an empty host creates one shell. It cannot be combined
with `--session`, `--plain`, or `--cwd`. `ctl ctmux --resume` and standalone
`ctmux --resume` open the same manager and cannot be combined with another
ctmux subcommand. For
noninteractive agent work, use detached creation, `ctl exec`, or a managed
background task instead of opening an interactive shell.

`ctl shell` and standalone `ctmux` use the shared TUI; detach with **Ctrl+B**,
then **d**. `ctl ctmux attach` uses the single-terminal presenter and detaches
with **Ctrl+]**. Detach leaves the session running and releases its attachment
leases. Inside the TUI, **Ctrl+B s/w** opens the session manager: Enter opens,
`n` creates, `r` renames, and `x` confirms whole-session termination. Leave the
new-session name blank and press Enter for a unique daemon-generated name;
blank or whitespace-only creation names are treated as omitted. Rename still
requires a nonempty valid name. Mutations
are unavailable in read-only mode. Renaming preserves running work and requires
ctmux 1.1.20; cancellation never terminates work.

Normal attachment requests input ownership without stealing another client's
lease. `--read-only` requests no input lease. Layout ownership is separate:
`--resize` requests it and can change shared terminal geometry. A transport
loss retains the logical attachment briefly for reconnection while `ctmuxd`
lives. Daemon-owned sessions survive client exit, not daemon loss.

Attachment emits terminal/control bytes and may wait indefinitely; stdin EOF
detaches it. For finite output and status use `ctl exec`; for background logs
use [ctl-task](../ctl-task/SKILL.md), available with `ctl skill ctl-task`.

## Work with panes and end sessions

Get terminal IDs from `view` before issuing terminal operations:

```sh
ctl -H work ctmux view development
ctl -H work ctmux split TERMINAL_ID
ctl -H work ctmux split TERMINAL_ID --vertical
ctl -H work ctmux promote TERMINAL_ID --name scratch
ctl -H work ctmux merge scratch development
ctl -H work ctmux kill-terminal TERMINAL_ID
ctl -H work ctmux rename-session -t development build
ctl -H work ctmux kill build
```

Split inherits the source terminal's known cwd unless overridden. Promotion and
merging preserve processes and terminal IDs. `kill-terminal` ends one terminal;
`kill` terminates every terminal in the selected root for all clients. Choose
the operation matching the user's requested scope; detachment does not require
termination. Retained archives are client-local text and cannot revive a process.

For Windows SSH targets add `--remote-platform windows` before `ctmux` and use
`-- cmd.exe /D /Q` when creating a command shell. For missing helpers or service
mismatches, consult [ctl setup](../ctl/references/setup.md) with
`ctl skill --file references/setup.md`.
