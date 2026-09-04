# Proposal 0003: Managed tasks in ctl

- Status: Proposed
- Created: 2026-09-04

## Summary

`ctl task` manages reusable commands and their executions on local or remote
hosts. A per-user `taskd` owns task registration, desired state, execution
policy, and run history. Tasks have explicit interactive and background modes
so terminal processes remain owned by rmux while noninteractive processes and
logs remain owned by taskd.

The desktop app workspace may save reusable task definitions and references to
managed tasks. A task may also be created directly in taskd without first being
saved in a workspace.

## Motivation

Users need a simpler alternative to a system service manager for development
servers, long-running commands, and finite jobs. A task should remain known
after it stops, retain useful run state, and be runnable again without
re-entering its command. Interactive commands should retain the terminal
behavior and attachment model already provided by rmux.

Task definitions, managed tasks, and concrete executions have different
lifetimes. Keeping them distinct prevents a workspace edit from silently
changing a registered task or an active process.

## Concepts

### Task definition

A reusable configuration describing what to execute. At minimum it includes a
display name, command, arguments, working directory, execution mode, and
restart policy. Environment and other execution options require explicit
storage and security rules before acceptance.

A definition in the desktop workspace is reusable configuration rather than
live daemon state.

### Managed task

A task registered with taskd under a stable task ID. It has a definition
snapshot, desired state, and references to its runs. It remains registered
while stopped and after a run completes.

A managed task may originate from a workspace definition or be created
directly through the CLI or app. Editing a workspace definition does not
implicitly mutate an existing managed task. Applying a changed definition is
an explicit operation.

### Run

One execution of a managed task under a stable run ID. A run records the exact
definition snapshot it used, lifecycle timestamps, outcome, and the
mode-specific execution reference. Restarting a task creates a new run rather
than erasing the previous run.

## Execution modes

### Interactive

An interactive task runs inside an rmux session. Taskd decides what and when to
run, then asks rmuxd to create the session with task and run metadata. Rmuxd
owns the PTY, child process, ordered terminal output, geometry, input, and
process exit. Users view and control the run through normal rmux attachments.

Taskd records the returned rmux session ID and observes its lifecycle. It does
not proxy or duplicate the terminal stream. After taskd restarts, it can
reconcile persisted runs with rmux sessions tagged by task ID and run ID.

### Background

A background task runs without a PTY. Taskd launches and owns the child process,
captures stdout and stderr as ordered log events, observes exit, and implements
termination. Users inspect its output through task log commands and the desktop
app rather than through an rmux attachment.

Background mode preserves stdout and stderr as separate streams. It does not
support terminal input, terminal geometry, or terminal control sequences as an
interactive contract.

## Ownership

Taskd is the logical authority for both execution modes:

- registered tasks and their definition snapshots;
- desired state and restart policy;
- run identity, lifecycle, and outcome;
- reconciliation after daemon restart.

Execution ownership depends on mode:

| Concern | Interactive task | Background task |
| --- | --- | --- |
| Child process | rmuxd | taskd |
| PTY | rmuxd | None |
| Canonical output | rmuxd terminal journal | taskd stdout/stderr log |
| Input and geometry | rmux leases | Not supported |
| Definition and policy | taskd | taskd |
| Run history | taskd | taskd |

Every PTY is owned by rmuxd. Taskd must not implement a second terminal
multiplexer or collect an interactive run's terminal output as a second
authoritative journal.

## Lifecycle

Starting a stopped managed task creates a run from its current definition
snapshot. For an interactive task, taskd requests a new rmux session. For a
background task, taskd starts a process with pipes. Taskd records the run as
running only after the corresponding execution owner accepts creation.

When a process exits, its execution owner supplies the outcome to taskd. Taskd
records the completed run and evaluates the restart policy. A restart creates a
new run. Stopping a task changes its desired state first, then requests
termination from the execution owner so an intentional stop cannot trigger an
automatic restart.

Operations must be idempotent across client retries. A lost response must not
create an unbounded number of runs.

## Desktop workspace

The existing desktop workspace may store:

- reusable task definitions;
- host-scoped references to managed task IDs;
- presentation choices such as ordering and selection.

It does not own live status, desired state, run history, process IDs, output,
or restart decisions. Those values come from taskd. Equal task IDs on different
hosts are distinct and use the same stable host identity model as rmux session
references.

Saving a directly created task into the workspace copies its definition as a
reusable workspace definition. Removing a workspace definition or reference
does not stop or unregister the managed task.

## CLI direction

The initial command surface should support the following operations, subject to
refinement during implementation:

```text
ctl task create <name> [options] -- <command> [arguments...]
ctl task list
ctl task show <task>
ctl task start <task>
ctl task stop <task>
ctl task restart <task>
ctl task logs <task> [--follow]
ctl task remove <task>
```

The global `--host` option selects the target consistently with `ctl rmux`.
Interactive runs expose their rmux session identity so `ctl rmux attach` and
the desktop app can attach without a second terminal interface.

## Remote boundary

Remote task control must continue to rely on OpenSSH authentication and fixed
remote commands. It must not make taskd a network listener or expose an
arbitrary local endpoint. The exact gateway shape is unresolved: extending
ctl-agent with an explicit service selector and creating a separate fixed task relay
are both possible. Raw task requests must not be confused with `rmux-proto`, and
the rmuxd maintenance endpoint remains local-only.

## Invariants

1. Taskd owns task identity, definitions, desired state, policy, and run
   history in both modes.
2. Every PTY and every process connected to one is owned by rmuxd.
3. Taskd owns background processes and their stdout/stderr logs.
4. A run uses an immutable definition snapshot.
5. Restart creates a new run with a new run ID.
6. Workspace edits do not silently change managed tasks or active runs.
7. Removing a workspace entry does not stop or unregister a managed task.
8. Interactive output has one canonical journal owned by rmuxd.
9. Background output has one canonical log owned by taskd.
10. Remote access uses OpenSSH and a fixed, explicitly scoped gateway.

## Out of scope

The first implementation does not need dependency graphs, timers, calendar
scheduling, privileged system services, multi-user task sharing, containers,
or arbitrary remote command execution. These features require separate design
proposals if later needed.

## Unresolved questions

1. Which task and run records are persisted, where they are stored, and what
   atomicity and migration rules the storage format requires.
2. Whether a keep-running task resumes automatically after taskd restart,
   login, or machine reboot, and how platform service registration works.
3. What happens to a background child when taskd crashes, and whether recovery
   restarts it or supports adoption.
4. How taskd receives reliable interactive-session exit events and reconciles
   sessions that end while taskd is unavailable.
5. How rmux session metadata exposes task ID and run ID without turning rmux
   into the task authority.
6. The restart-policy model, retry limits, backoff, and successful-exit
   semantics.
7. Background log retention, ordering, rotation, following, and redaction.
8. Environment-variable storage, secret handling, and inheritance rules.
9. The exact remote task gateway and protocol framing.
10. Whether task names are unique globally within one taskd or may be grouped
    into namespaces.

## Implementation status

The first local background slice is implemented. It includes the versioned
task protocol, persisted task and run records, taskd auto-start, background
process groups on Unix and Job Objects on Windows, bounded in-memory stdout/stderr logs, log following, and the
local `ctl task` command surface. Interactive rmux-backed execution, SSH
routing, persistent logs, restart policies, and desktop workspace integration
remain pending, so this proposal stays **Proposed**.

## Detailed specifications

- [Proposal 0001: Persistent terminal sessions with rmux](0001-rmux.md)
- [Proposal 0002: Local and SSH control routing with ctl](0002-ctl.md)
- [Architecture](../architecture.md)
- [rmux protocol](../rmux-protocol.md)
- [ctl SSH transport](../ctl-protocol.md)

### Windows background execution

Windows taskd uses a local named pipe with an owner-only DACL and remote clients
rejected. Job assignment occurs before the new process resumes. A run ends with
its root process; remaining descendants are terminated before logs finish.
Stop terminates the job immediately, with no Unix signal emulation. Closing
or crashing taskd kills its jobs. On recovery, previously active runs become
failed and stopped; they are not adopted or automatically restarted.

The state directory has a lifetime exclusive file lock, preventing multiple
endpoints from writing the same state. State replacement uses the platform's
rename operation. Windows files inherit the data directory's ACL; custom
locations must retain user-private access. PTY tasks remain reserved for rmuxd.
