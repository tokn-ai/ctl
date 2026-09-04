# Proposal 0007: Local task definitions, runs, and schedules

- Status: Proposed
- Created: 2026-09-05

## Summary

Make local `ctl task` useful for one-off commands, reusable command shortcuts,
long-running processes, and scheduled scripts. A saved task definition describes
work; a run is one execution with its own identity, context, output, and result.
Saving a definition, adding a schedule, and requesting automatic restart are
independent choices.

The intended progression is: run a command, leave it running if needed, save it
for reuse if useful, and optionally add a schedule. A one-off run does not require
creating a permanent named task first.

This proposal extends [Proposal 0003](0003-task-system.md) and revises the local
invocation and singleton assumptions in [Proposal 0005](0005-desktop-tasks.md).
On acceptance, the behavior below takes precedence for new local workflows.
Existing behavior remains current until the corresponding migration is implemented.
The SSH service boundary in [Proposal 0006](0006-remote-tasks.md) is preserved;
new remote invocation semantics require a separate decision.

Adopting an arbitrary running shell job with Ctrl+Z followed by `task bg` is
explicitly deferred for separate investigation. This proposal covers commands
launched under task management from the beginning.

## Motivation

The current local CLI captures the working directory when creating a task,
requires a name, and allows one active run per registered task. This suits a
named development server with a fixed directory. It is awkward for a `build`
shortcut used in several projects, a command worth running only once, or a
script invoked repeatedly by a timer.

The current protocol already distinguishes definitions and runs, and rmux
already provides terminal ownership and attachment. Extend those foundations
while making execution context, reuse, and lifecycle policy explicit.

| Use | Saved definition | Trigger | Typical directory |
| --- | --- | --- | --- |
| Scheduled script | Required | Schedule | Fixed |
| Long-running server or command | Optional | Manual | Selected at launch |
| Common-command shortcut | Required | Manual | Invocation directory or fixed |
| One-off command | Optional | Manual | Invocation directory |

## Design

### Saved definitions and runs

A saved definition has a stable definition ID, a revision, a scoped name, a
program and arguments, a working-directory policy, and an execution backend.
It provides defaults for future invocations. Editing it never changes the
command or context of an existing run.

Each invocation creates an independent run with a stable run ID. A run records
its source definition and revision when present, the actual executable and
arguments, the resolved absolute working directory, its trigger, lifecycle
timestamps, result, and execution reference. Submitted definition snapshots
and resolved execution context are separate so dynamic directory policies do
not overwrite the original definition.

A one-off invocation supplies its command directly and receives a run ID without
saving a definition. It may have a display label, but does not need a unique
permanent name. Users can save a definition from a run afterward, explicitly
choosing which execution settings should become reusable defaults.

Run IDs are authoritative for inspection, output, attachment, and stopping.
The same definition can have several active runs, including runs in different
directories. An operation addressed by name must resolve unambiguously or
request a run selection; it must never stop an arbitrary matching run.

Reusable means that a definition can be invoked again. A schedule creates
invocations automatically. A restart policy creates another run after an exit.
These behaviors do not use a shared `repeatable` flag.

### Invocation context and directories

Definitions support two directory policies:

- **Invocation directory**: resolve the directory from the caller every time
  the definition is invoked. Creating or saving the definition does not bind it
  to the directory used at that moment.
- **Fixed directory**: use a stored directory regardless of where the caller
  invokes the definition. Resolve relative input against the caller's directory
  when binding it and store the resulting absolute path.

A manual run may explicitly override the directory for that invocation. The
override is recorded in the run and does not edit the saved definition. Never
fall back to taskd's own current directory when invocation context is missing.

For example, a global `build` definition using the invocation directory can run
in `/work/app-a` and `/work/app-b` simultaneously. A fixed-directory `api` always
starts in `/work/api`, including when invoked from another folder.

A schedule has no calling terminal. It must bind a fixed directory, either from
the definition or from an explicit schedule setting. Scheduling an
invocation-directory definition without that binding is an error. Binding the
schedule does not change how manual invocations resolve the same definition.
Missing directories fail visibly instead of falling back to the user's home.

Manual invocation must also define executable lookup, environment inheritance,
and argument overrides. Today background processes inherit taskd's environment;
that does not reliably match the invoking shell. The request should carry the
required invocation context, with an explicit environment policy for scheduled
runs. The exact inheritance and persistence rules remain an acceptance decision.

Programs and argument arrays remain the basic execution form. Shell expressions
can use an explicitly selected shell. Shortcuts that change the calling shell,
such as `cd` or `export`, require shell integration and are outside this proposal.

### Execution and attachment

Keep terminal requirements separate from whether the user is currently watching
the run:

- **Terminal backend**: rmuxd owns the PTY, process, terminal output, and input.
  A client may attach or detach without changing that backend.
- **Pipe backend**: taskd owns the process and stdout/stderr logs. A client may
  wait for completion or follow logs, then stop following while execution continues.

A user can launch a managed command in the foreground and detach when it takes
longer than expected. The process continues with the same run ID and backend.
Reattaching a terminal run restores its existing terminal; following a pipe run
opens its logs. Detaching does not attempt to replace a PTY with pipes.

Explicit stop controls execution. Closing a view or intentionally detaching only
closes the client connection. An unexpected client disconnect also leaves the
daemon-owned run running. Surviving terminal closure does not imply surviving
daemon failure, logout, or machine reboot.

The exact foreground CLI controls, signal forwarding, and detach gesture need a
small interaction specification before implementation. They must preserve a
way to interrupt work intentionally without treating every client exit as stop.

### Schedules, concurrency, and restart

A schedule is a persisted trigger referencing a saved definition by stable ID,
with an enabled state and a complete execution-context binding. It creates
ordinary runs whose trigger identifies the schedule and occurrence. Renaming a
definition does not break the reference, and deleting a referenced definition
must require resolving its schedules first.

The proposed revision behavior is that future occurrences use the latest saved
definition. Each resulting run records the exact revision used. An edit to the
definition leaves active runs unchanged. Schedule-bound context, such as its
fixed directory, remains explicit and is shown alongside definition defaults.

Concurrency is a policy of an invocation source or managed instance, rather
than a single active-run slot shared by every use of a definition. Manual
shortcut invocations may run independently. A schedule chooses an overlap
policy for its own unfinished invocations: skip, queue, or allow overlap. The
proposed initial default is skip, with a visible skipped-occurrence record.
Queue limits and behavior must be specified before offering queue as an option.

Scheduling also needs explicit timezone, missed-occurrence, and clock-change
semantics. The proposed first version skips missed occurrences after sleep or
downtime rather than replaying an unbounded backlog. Whether to support calendar
expressions, elapsed intervals, or both remains open; these have different
timing semantics.

Automatic restart is opt-in, with no restart as the default. A long-running
process does not acquire a restart policy merely because it stays alive or the
client detaches. When configured, restart belongs to an explicit managed
invocation or service instance and creates a new run ID after an exit. Stopping
an active run in that instance cancels the instance's restart intent before
terminating execution, so an explicit Stop cannot immediately relaunch the work.
Such runs carry the managed-instance identity in their execution reference.
Stopping a run does not stop independent invocations or disable future schedule
occurrences. Disabling a schedule prevents future occurrences without stopping
its active runs. Restart backoff, successful-exit behavior, and interaction with
schedules must be specified before automatic restart is implemented.

Taskd owns scheduling and restart decisions. The design must persist enough
occurrence identity and run intent to reconcile uncertain launch results and
avoid starting duplicate runs after reconnect or daemon restart. It must not
claim exactly-once external effects for arbitrary commands. Automatic daemon
startup at login or boot is a separate integration; scheduling initially
requires taskd to be available. Enabled schedules keep a running taskd from
exiting for idleness while it waits for the next occurrence.

### Names, storage, and history

The proposed naming direction supports project definitions and user-global
definitions. An unqualified name uses the project's definition when present,
then falls back to the global definition. Explicit scope selection remains
available. The shared storage implementation now follows this scope model;
the complete invocation and scheduling design remains proposed.

Names identify definitions within a scope. They are not run IDs and do not need
to become unique names such as `build-2` just to permit another invocation.
Scheduled references bind stable definition identity and project context at
configuration time; they do not rediscover a project from taskd's directory.

Taskd remains authoritative for runs, lifecycle state, schedules, and execution
policy. Definitions must be usable from the CLI without requiring the desktop
app to be running. [Shared task definitions](../task-definitions.md) specifies
the implemented JSON catalogs, content revisions, desktop integration, and migration.

Keep completed-run metadata and output under an explicit retention policy,
including one-off runs. History retention and saved-definition lifetime are
independent: saving a command does not retain its output forever, and finishing
an unsaved command does not immediately discard its result. Persistent background
logs and useful run history are required for unattended scheduled work. Limits,
rotation, and cleanup behavior belong in the storage specification.

### CLI and desktop direction

The primary actions are **Run command**, **Run saved definition**, **Save
definition**, **Schedule**, and run-scoped **Inspect**, **Follow output**,
**Attach**, and **Stop**. Foreground and detached launch are invocation choices.
These illustrative forms describe the CLI direction. Saving from a retained run
is implemented; the new `run` and `schedule` commands remain proposed:

```text
ctl task run -- <program> [arguments...]
ctl task run <definition> [invocation-options]
ctl task save <name> --from-run <run-id>
ctl task schedule <definition> [schedule-options]
```

The desktop should distinguish saved definitions from active and recent runs.
Opening a run focuses that run's view without starting another execution. A
separate Run action invokes a definition again. Existing named service instances
may retain a stable view across explicit restarts, while shortcut invocations
receive independent run views. Selecting a directory for a desktop invocation
must be explicit when no project context is available.

### Compatibility and migration

The existing implementation provides task registration, one active and latest
run per task, persisted metadata, bounded in-memory background logs, and rmux
attachment. It does not yet provide these scheduling or invocation policies.

Preserve existing task IDs, names, stopped/running state, run snapshots, and
desktop references. Existing registered tasks keep their fixed directories and
singleton behavior when configured that way. Preserve omitted and relative
directory values and their existing host-home resolution as well; not every
legacy definition contains a fixed absolute path. Introducing invocation-directory
defaults must not change where an existing task runs. A compatibility adapter
can retain their managed instance identity while new definitions support
independent invocations.

Do not mutate or restart existing processes during migration. In particular,
keep managed rmux session ownership and task/run identities intact. Define a
versioned protocol and recoverable storage migration before changing the current
registration model. Legacy create/start commands retain their current semantics
until an explicit migration or replacement is documented.

This is a local workflow proposal. Existing remote requests retain their current
directory resolution and fixed gateway commands. A local caller's directory or
environment must not silently be sent as valid remote execution context.

## Invariants

1. Every invocation has an independent run ID and an immutable command/context snapshot.
2. A one-off run does not require a permanent name or saved definition.
3. Directory policies resolve at the documented boundary, with no daemon-cwd fallback.
4. Automatic invocations have a complete context without a calling terminal.
5. Definition edits affect future invocations, never active runs or recorded results.
6. Terminal processes and output remain owned by rmuxd; pipe execution remains in taskd.
7. Detaching preserves the process, run identity, and execution backend.
8. Reuse, scheduling, restart, concurrency, and history retention are separate choices.
9. Ambiguous names never select an arbitrary run for a destructive operation.
10. Existing tasks and sessions retain their behavior throughout migration.

## Acceptance checks

1. A command runs once without creating a saved definition; its outcome remains inspectable.
2. The same invocation-directory definition runs concurrently in two folders with distinct IDs.
3. A fixed-directory definition uses its stored directory from any calling folder.
4. A directory override affects only that run, and history records the resolved context.
5. Saving a completed run creates reusable configuration without altering the run record.
6. Detaching a managed terminal run and reattaching preserves the process and terminal.
7. Stopping log following leaves a pipe run alive; explicit Stop terminates the selected run.
8. A schedule without a required directory binding is rejected before activation.
9. Scheduled runs record definition revision and trigger identity; overlap and missed runs
   follow the selected policy, including across daemon recovery.
10. Editing a definition does not relabel prior runs or modify active execution.
11. Migrating existing tasks preserves directory behavior, IDs, live sessions, and references.
12. An ambiguous name requires selection, and arbitrary shell-job adoption is not implied.

## Out of scope

- Ctrl+Z followed by `task bg`, adopting an arbitrary PID or shell job, moving
  that job to another terminal, or extracting one job from an existing rmux shell.
  Investigate these together with shell integration, process groups, terminal
  ownership, reliable exit observation, and platform constraints in a later proposal.
- New remote invocation-context or scheduling behavior.
- Shell aliases that mutate their calling shell, dependency graphs, distributed
  scheduling, privileged services, and machine provisioning.
- Automatic login/boot service installation and transparent process survival
  across machine reboot or daemon failure.

## Unresolved questions

1. Complete definition/run identity and directory-policy evolution beyond the
   implemented shared project/global catalogs, preserving existing registrations.
2. Specify manual and scheduled environment inheritance, executable lookup,
   argument overrides, and what context is persisted for inspection or rerun.
3. Specify foreground CLI defaults, detach controls, signal behavior, and how
   existing create/start commands coexist with run-oriented commands.
4. Choose schedule syntax and timezone/clock semantics, missed-occurrence policy,
   overlap options, and the treatment of definition edits while occurrences are queued.
5. Specify restart intent, backoff and limits, and its interaction with schedule
   concurrency before implementing automatic restart.
6. Specify durable run/log retention and protocol/storage migration, including
   recovery from uncertain process creation without duplicate launches.

## Detailed specifications

- [Shared task definitions and migration](../task-definitions.md)
- [Managed tasks](0003-task-system.md)
- [Tasks in the desktop workspace](0005-desktop-tasks.md)
- [Explicit task routing over SSH](0006-remote-tasks.md)
- [Architecture](../architecture.md)
- [rmux local lifecycle control](../rmux-local-control.md)
