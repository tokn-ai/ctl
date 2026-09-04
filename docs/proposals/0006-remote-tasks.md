# Proposal 0006: Explicit task routing over SSH

- Status: Implemented
- Created: 2026-09-04

## Summary

Route `ctl --host HOST task ...` through a fixed task service in `ctl-agent`.
Task registration, lifecycle, and background logs use taskd on the selected
host. Interactive attachment uses rmux on that same host. This supersedes the
rmux-only service restriction in [Proposal 0002](0002-ctl.md) and extends
[Proposal 0004](0004-windows-ssh.md) to the task service.

## Motivation

The CLI already selects SSH targets for terminals, and tasks already have an
independent daemon and protocol. Remote tasks need the same target selection
without exposing arbitrary sockets or changing who owns execution. Docker
targets must package both services so interactive tasks can create their PTYs.

## Design

The gateway accepts a closed service enum. `ctl-agent connect` retains the rmux
default; `ctl-agent connect --service task` selects the authenticated user's
fixed taskd endpoint. Unix remote commands begin with `exec`; Windows commands
use `ctl-agent.exe` with the default cmd.exe SSH shell. Each gateway connects to
an existing daemon first and starts only the installed companion when needed.
Windows daemon startup detaches and requests SSH job breakaway.

Both services emit the existing `ctl-ssh-v1` readiness marker before relaying
raw bytes. The client then validates the selected domain's protocol handshake.
The gateway does not parse requests, invent a multiplexing protocol, or accept
an endpoint path. Rmux's maintenance endpoint remains local-only; taskd uses it
locally for managed interactive sessions.

Each task CLI request uses the original selected target, including the start
following `create --start`. `task attach` reads the interactive session ID from
that host's taskd, then opens the ordinary rmux transport on the same target.
Remote endpoint paths returned in metadata are never interpreted as local paths.

Local creates retain the caller's working directory. Remote creates with no
working directory use the remote user's home. Explicit absolute paths are
remote paths; relative paths resolve against the remote home. Taskd resolves
working directories when starting execution, while preserving submitted
definitions and run snapshots so saved-definition comparisons remain stable.

The local CLI currently captures cwd at task creation, rather than at each
later start. [Proposal 0007](0007-local-task-workflows.md) proposes independent
local runs with caller or fixed cwd and local scheduling; it remains
**Proposed**. It does not change this implemented SSH gateway boundary or
extend the new local workflows to remote hosts.

The Docker image installs `ctl-agent`, `ctl`, `rmuxd`, and `taskd`. Its forced
command allowlist accepts exactly `exec ctl-agent connect` and
`exec ctl-agent connect --service task`, mapping each to literal arguments
without evaluating SSH input. The private task endpoint lives in `/run/taskd`;
task metadata is stored in a named volume at `/var/lib/taskd`, owned by the
rmux account. Taskd and the rmux gateway share `/run/rmux`.

Task definitions and active/latest run records persist. Background logs and
terminal output do not. Container replacement ends processes and terminals;
taskd reconciles interrupted runs without automatically recreating them.

## Invariants

1. SSH owns authentication, host verification, and the remote account authority.
2. Service selection is an enum; arbitrary commands and socket paths are rejected.
3. The rmux default command and wire protocol remain compatible.
4. Taskd owns task state and background execution; rmuxd owns all task PTYs.
5. Interactive attachment and every follow-up task request retain the same target.
6. Gateway disconnect ends only the relay, leaving daemon-owned work running.
7. Rmux maintenance is never directly relayed over SSH.
8. A container's persistent task metadata does not imply persistent processes.

## Out of scope

Desktop remote task UI, automatic task restart policies, persistent background
logs, full run history, arbitrary gateway service registration, remote daemon
maintenance, file synchronization, and custom Windows SSH shells.

## Unresolved questions

None for this CLI and Docker boundary. Native Windows SSH validation belongs
in the Windows CI fixture; other server shells remain outside the supported
command convention.

## Detailed specifications

- [Managed tasks](0003-task-system.md)
- [Proposed local task workflows](0007-local-task-workflows.md)
- [ctl SSH transport](../ctl-protocol.md)
- [Remote setup and Docker upgrades](../remote-mvp.md)
- [rmux local lifecycle control](../rmux-local-control.md)
