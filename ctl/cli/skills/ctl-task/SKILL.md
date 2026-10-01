---
name: ctl-task
description: Manage ctl registered background or interactive tasks and their lifecycle, status, and logs. Use for ctl task commands and reusable local task definitions.
---

# ctl tasks

Use this skill for `ctl task`. For saving reusable commands, catalog changes,
`--from-definition`, or `--from-run`, read
[references/definitions.md](references/definitions.md) with
`ctl skill ctl-task --file references/definitions.md`.

## Choose the process model

| Need | Command |
| --- | --- |
| Run once and preserve the command's exit status | `ctl exec -- PROGRAM ARGS...` |
| Keep a terminal session through disconnects | `ctl ctmux new --detached --name NAME --cwd /absolute/path -- PROGRAM ARGS...` |
| Register a named process with start/stop/restart and background logs | `ctl task create NAME --cwd /absolute/path --start -- PROGRAM ARGS...` |
| Manage a process that requires terminal input or a PTY | Add `--mode interactive` to task creation |

A registered task retains its definition and latest run metadata in ctl-taskd.
Registration and execution are separate: creation starts only with `--start`.
Each task has at most one active run. Names are unique within the selected
host's ctl-taskd; task selectors accept a name or task ID. Inspect existing tasks
before creating another registration for the same work.

## Select the target and command

Omitting `--host` uses local ctl-taskd. For SSH, carry the same host and connection
method through creation, inspection, logs, attachment, and lifecycle commands:

```sh
ctl -H work --method VPN task list
ctl -H work --method VPN task create api --cwd /srv/api --start -- cargo run
ctl -H work --method VPN task show api
ctl -H work --method VPN task logs api
```

Omit `--method` to use the saved host's preferred method. Host values select a
saved ctl host by name or ID, then fall back to an OpenSSH destination or alias.
A task ID belongs to its target; it does not select a host by itself.

For direct local creation, cwd defaults to the caller's current directory and
relative `--cwd` resolves there. For remote creation, omitted cwd means the
remote user's home; relative paths resolve against that home. Use an existing
absolute target path when the location matters. `ctl exec` has no `--cwd`;
use an explicit shell command to change directory in a one-off remote command.

Arguments after `--` are a program and argv, without shell expansion. Invoke
`sh -c '...'` explicitly for shell syntax. Background mode is the default:
stdin is closed, stdout/stderr are captured through pipes, and no PTY is
allocated. Commands needing prompts, terminal control, or continued input
belong in interactive mode. Use `cmd.exe /D /Q` for a Windows command shell;
Windows SSH targets require `--remote-platform windows` and the default
cmd.exe SSH shell.

## Manage a run

```sh
ctl task create api --cwd /srv/api -- cargo run
ctl task start api
ctl task show api
ctl task logs api
ctl task logs api --follow
ctl task restart api
ctl task stop api
ctl task remove api
```

`start` rejects an active run. `restart` stops one if necessary, then starts a
new run. `stop` requires an active run; `remove` requires it to be stopped.
Removing a registration discards its stored task metadata. There is no task
definition-edit CLI; when a registered command must change, stop it and
recreate the registration with the desired command. Check the existing state
before choosing which lifecycle operation to perform.

Background stop targets the process group on Unix, escalating if needed.
On Windows it terminates the Job Object's process tree. When the root process
finishes, remaining background descendants are cleaned up. Interactive stop
terminates the ctmux-owned session. Starting and restarting are explicit;
automatic restart and scheduling are not implemented.

Background logs select the active run, otherwise the latest run. `--follow`
ends with that run and does not switch to subsequent runs. `--after SEQUENCE`
skips events through a known log sequence, but the CLI prints raw log bytes
without their sequence numbers. Logs are bounded to about 4 MiB per run and
held in memory, so daemon restart loses them. Interactive tasks use attachment
instead of `task logs`:

```sh
ctl task create console --mode interactive --cwd /srv/api --start -- bash
ctl task attach console
```

`task attach` needs an active interactive run. It requests input ownership and
layout ownership; acquiring layout can resize the shared PTY. It never takes
leases from an existing owner. Detach with **Ctrl+]** to keep the process
running and release leases. Unexpected transport loss preserves the session
and leases for reconnect grace, normally 30 seconds. EOF on attachment stdin
also detaches, so attachment is not a finite log capture command. `task show`
prints the session ID for a separate `ctl ctmux attach SESSION_ID`, where
`--read-only` and optional `--resize` provide explicit attachment choices.

## Interpret results and daemon limits

Registered-task commands print task ID, name, state, and program, plus an
optional ctmux session line. They have no `--json` option and do not print argv,
cwd, run ID, or numeric exit code. A successful control command means the
request succeeded; it does not mean the managed program completed successfully.
Inspect `task show` after completion: `completed` means exit 0; `failed` can
include an unknown exit status. Logs and attachment do not propagate the
child's exit code. Use `ctl exec` when the exact exit status is required.

`starting` or `unknown` represents an active run awaiting creation or
reconciliation; inspect it rather than attempting repeated starts. Taskd
persists registrations and latest results, but background processes depend on
ctl-taskd. Interactive runs can reconcile after ctl-taskd restart while the same ctmuxd
instance remains alive; losing or replacing ctmuxd fails affected runs without
automatically recreating them.

Local tasks require ctl-taskd; interactive tasks also require ctmuxd. Remote task
operations require ctl-agent and those daemons on the destination, plus ctld
on Unix clients. Helpers start on demand and must match their clients. Check
`CTL_TASKD_BIN`, `CTMUXD_BIN`, or `CTLD_BIN` overrides when startup/version errors
point to an unexpected helper. `ctl taskd restart` controls only local ctl-taskd,
starts it if absent, and refuses active tasks. An ctmuxd restart terminates its
sessions; do not use it as a routine recovery for running work.
