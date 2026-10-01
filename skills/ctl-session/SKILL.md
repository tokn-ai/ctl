---
name: ctl-session
description: "Create, attach to, inspect, split, merge, or terminate persistent local or SSH terminal sessions with ctl shell and ctl rmux. Use when work needs a persistent PTY; managed task lifecycle and background logs belong to ctl-task."
---

# ctl Sessions

Use `ctl shell` for an interactive persistent shell, and `ctl rmux` for session
and terminal operations. Omit `-H` for local work; use `ctl -H HOST [--method
NAME_OR_ID] ...` for a saved host, SSH alias, or destination. Keep the same
target and selected method throughout the workflow. Local sessions require
`rmuxd`; remote sessions require `ctl-agent` and `rmuxd` for the SSH account,
with `ctld` on Unix clients.

## Create and inspect

```sh
ctl rmux list
ctl rmux new --detached --name development --cwd /absolute/project
ctl -H work rmux new --detached --name build --cwd /srv/app -- cargo build
ctl -H work rmux list
ctl -H work rmux view build
ctl -H work rmux state build
```

Inspect inventory before reusing or terminating a session; use returned IDs
when names are ambiguous. `ctl rmux new` returns a session ID and name without
attaching in the current implementation; spell `--detached` explicitly in
automation. Standalone `rmux new` attaches by default.

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
ctl -H work rmux attach development
ctl -H work rmux attach development --read-only
ctl -H work rmux attach development --resize
```

`ctl shell` requires terminal stdin and stdout. Without `--session` it creates
a new session; a supplied name attaches if present or creates if absent. For
noninteractive agent work, use detached creation, `ctl exec`, or a managed
background task instead of opening an interactive shell.

Both `ctl shell` and `ctl rmux attach` use the single-terminal presenter.
Detach with **Ctrl+]**. Detach leaves the session running and releases its
attachment leases. Standalone `rmux` uses the TUI by default, where detachment
is **Ctrl+B**, then **d**; do not apply that shortcut to ctl attachments.

Normal attachment requests input ownership without stealing another client's
lease. `--read-only` requests no input lease. Layout ownership is separate:
`--resize` requests it and can change shared terminal geometry. A transport
loss retains the logical attachment briefly for reconnection while `rmuxd`
lives. Daemon-owned sessions survive client exit, not daemon loss.

Attachment emits terminal/control bytes and may wait indefinitely; stdin EOF
detaches it. For finite output and status use `ctl exec`; for background logs
use [ctl-task](../ctl-task/SKILL.md), available with `ctl skill ctl-task`.

## Work with panes and end sessions

Get terminal IDs from `view` before issuing terminal operations:

```sh
ctl -H work rmux view development
ctl -H work rmux split TERMINAL_ID
ctl -H work rmux split TERMINAL_ID --vertical
ctl -H work rmux promote TERMINAL_ID --name scratch
ctl -H work rmux merge scratch development
ctl -H work rmux kill-terminal TERMINAL_ID
ctl -H work rmux kill development
```

Split inherits the source terminal's known cwd unless overridden. Promotion and
merging preserve processes and terminal IDs. `kill-terminal` ends one terminal;
`kill` terminates every terminal in the selected root for all clients. Choose
the operation matching the user's requested scope; detachment does not require
termination. Retained archives are client-local text and cannot revive a process.

For Windows SSH targets add `--remote-platform windows` before `rmux` and use
`-- cmd.exe /D /Q` when creating a command shell. For missing helpers or service
mismatches, consult [ctl setup](../ctl/references/setup.md) with
`ctl skill --file references/setup.md`.
