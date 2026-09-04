# Proposal 0005: Tasks in the desktop workspace

- Status: Accepted
- Created: 2026-09-04

## Summary

The desktop sidebar has a vertical activity bar with Sessions and Tasks tabs.
Terminals and managed task output share the main tab strip. Task creation and
definition editing use an in-app dialog over the current tab. A task tab represents a managed task, so stopping or restarting a run does
not close it or change its position.

The agreed interaction is one default managed task per saved definition and
host. A separate **Run another instance** action creates an independent task.
The first desktop implementation supports the local host. Remote controls in
the desktop remain separate UI work; the CLI gateway is defined in
[Proposal 0006](0006-remote-tasks.md). References include host identity from the
beginning.

This document describes the current default-task desktop interaction.
[Proposal 0007](0007-local-task-workflows.md) proposes extending saved definitions
to independent local runs, explicit caller or fixed working directories, and
local scheduling. That proposal remains **Proposed**. The default registration,
**Run another instance**, and task-tab behavior below remain current until the
new design is accepted and implemented.

## Screen plan

```text
Workspace                 | shell × | api × |              |
                          +--------------------------------+
Terminals                 | api · Local · Running           |
  shell                   | Background        Stop Restart |
                          | Command and working directory   |
Tasks                 [+] +--------------------------------+
  Saved definitions       | Output                          |
    api                   | [stdout] Listening on :3000     |
    build                 | [stderr] ...                    |
  Managed tasks           |                                |
    api       Running     | Follow output  Copy  Clear view |
    build     Completed   |                                |
```

- Selecting a managed task opens or focuses its tab without starting it.
- Selecting a saved definition opens its dialog. New drafts offer **Create definition**
  and **Create and run**; existing definitions offer **Save changes** and **Save and run**.
  Run remains available on saved definitions in the sidebar.
- The editor contains name, executable, arguments, working directory, and
  Background / Interactive mode. Arguments are entered as separate values;
  the app does not infer shell quoting or execute a command through a shell.
- First Run registers and starts the default managed task. Run on an already
  active task focuses its tab; it never implicitly restarts the task. Run on a
  stopped task starts a new run. Restart is always an explicit action.
- Run another instance asks for a unique task name and opens the new task tab.
  It does not replace the default registration.
- A task header shows name, host, mode, state, command, working directory, and
  the current/latest run's exit result. Start, Stop, and Restart availability
  follows taskd state; pending operations disable conflicting actions.
- Background output is a plain text log with distinguishable stdout/stderr,
  copy, follow, and clear-view controls. Clear view does not delete daemon logs.
  Old logs retained on screen are clearly separated when a new run starts.
- Interactive output uses the existing terminal renderer, leases, keyboard
  behavior, and attachment transport. No second terminal implementation is added.
- Opening an interactive task's session from the terminal list focuses its task
  tab. If an ordinary session tab is already open, adopt it in place as the task
  tab. Deduplication uses host plus session ID, and task identity survives restart.
- Closing a tab only closes its view. Removing a saved definition does not stop
  or unregister tasks. Removing a managed task requires it to be stopped and
  names that task explicitly. Forgetting a workspace reference is separate.
- Draft edits autosave to the workspace, including incomplete fields. Closing the
  dialog by Escape, its close button, or the backdrop dismisses it without a
  confirmation and retains the draft. Drafts can be resumed from the Tasks sidebar
  after an app restart. Creating a runnable definition and starting a task remain
  explicit actions. Failed writes are visible; normal app close flushes pending writes.

### Loading, errors, and restoration

An empty saved section offers **Create task definition**. The managed section
shows local taskd tasks, including tasks created through the CLI. Each can be
opened and explicitly saved as a reusable workspace definition.

Loading and unavailable states are distinct from an empty task list. A taskd
connection failure shows a retry action and preserves task references and tabs.
`starting` and `unknown` have explicit labels; unknown is never presented as
stopped. Failed actions remain visible beside the relevant task with an error
message, while the last observed state is retained.

Restoring a workspace restores presentation only. It neither starts nor restarts
tasks. A missing registered task offers explicit recreation from its saved
definition; an unavailable daemon is not treated as a missing task. Remote
targets explain that task control is not yet supported and never execute locally
as a fallback.

## Command plan

Add async Tauri task commands in a dedicated backend module, with invocation
wrappers in `src/lib/tauri.ts`. Use a reusable Rust task client speaking the
existing framed protocol; do not execute and parse the human-readable ctl CLI.

- List and inspect managed tasks.
- Register, start, stop, restart, and remove tasks.
- Apply a saved definition to a stopped managed task explicitly.
- Open and cancel a background log subscription. Deliver run-scoped log events
  over a Tauri channel; cancel subscriptions on tab changes, close, and window
  teardown. Bound retained frontend output and preserve incremental UTF-8 decoding.
- Reuse workspace load/update with its existing revision conflict handling for
  saved definitions, registration references, and presentation state.
- Reuse existing rmux inspection and attachment commands for interactive output.

Registration must be idempotent before the app retries it after a lost response.
Add a stable registration identity to taskd rather than using display names as
identity. Persist a pending registration reference before dispatch. Concurrent
windows must converge on the same registration or show a conflict; they must not
create duplicate default tasks. Additional instances receive new identities.

Changing a saved definition does not mutate a managed task. If they differ,
show **Saved definition has changes** with an explicit Apply action available
while stopped. Starting the registered command remains a separately labelled
choice. Applying requires a taskd update operation and immutable definition
snapshots in run records so previous results are not relabelled with a new command.

Use cancellable status refresh while the app is active. A slow or stale response
must not replace newer action results, switch the selected tab, or attach a
previous run. Errors from background subscriptions do not tear down unrelated tabs.

## Data model plan

Shared TypeScript types belong in `src/lib/types.ts`, with snake_case serialized
fields and matching Rust DTOs. Taskd remains authoritative for live status,
desired state, runs, exit results, and background logs. Rmuxd remains authoritative
for interactive processes and terminal output.

Migrate workspace schema version 1 to version 2 with:

- `task_definitions`: stable `definition_id`, editable command configuration,
  and definition revision.
- `task_references`: `host_id`, registration identity, optional `task_id`,
  optional `definition_id`, and the last applied definition revision.
- Tagged tab references: `session`, `task`, or `task_definition`. A task tab is
  keyed by host/task identity; its current session ID is resolved from taskd.
- A tagged active-tab reference and existing tab order.

The migration preserves existing hosts, terminal membership, tab order, and active
selection. Preserve a recoverable copy before replacing the old workspace file.
Runtime status, logs, terminal bytes, and process IDs are never workspace data.
Editor drafts autosave in the workspace, separately from runnable definitions;
they are never daemon state.

Keep task coordination, editors, log subscriptions, and generalized tab state in
feature modules. The existing large TerminalPage should compose these modules
rather than accumulate task protocol and persistence logic.

## Acceptance checks

1. Existing workspaces migrate without losing terminals or opening connections.
2. Run twice, including from concurrent windows, reuses the default registration.
3. Another instance creates a separate task and tab.
4. Stopped tasks open without starting; closing tabs leaves tasks running.
5. Restart keeps tab identity and switches output to the new run.
6. Interactive task/session navigation never opens duplicate terminal views.
7. Background following is bounded, run-scoped, cancellable, and preserves UTF-8.
8. Saving a definition does not change a registered task until explicit Apply.
9. Connection failures retain state, provide Retry, and never imply success.
10. Keyboard navigation and command-palette actions work for all tab kinds.

## References

- [Proposal 0003: Managed tasks in ctl](0003-task-system.md)
- [Proposal 0007: Local task definitions, runs, and schedules](0007-local-task-workflows.md)
- [Workspace implementation](../../apps/rmux/src-tauri/src/workspace/mod.rs)
- [rmux local-control protocol](../rmux-local-control.md)


## Sidebar and draft storage update

The current schema 3 update moves saved definitions into shared project/global
catalogs. The Tasks sidebar selects Global or an explicit project directory;
saves and deletes use native definition-store commands with the inspected
revision. Drafts retain their source scope and original revision, including
across catalog refresh and app relaunch. Workspace writes retain drafts and
references but never replace the shared catalog. See
[shared task definitions](../task-definitions.md) for migration and conflicts.
The schema 2 behavior below records the preceding draft-storage transition.

Workspace schema v2 additionally stores `sidebar_view` (Sessions by default) and
`task_drafts` (empty by default). Each draft has a stable `definition_id` and the
editable definition fields. Draft validation permits incomplete commands and
paths while bounding stored data. Existing definitions retain their stricter
validation and are changed only by an explicit save action.

Legacy definition tabs are removed from the restored tab strip; definitions
remain available in the sidebar and open in the dialog. Ordinary session and
managed-task tabs keep their relative order. The dialog does not detach a
terminal, and its focus containment and the app command barrier prevent input
from reaching the terminal while editing.


### Command-line entry

The task dialog focuses a command-line field by default. POSIX single/double
quotes and backslash escapes split it into the executable and argument rows.
Those rows remain editable and regenerate an equivalent quoted command line.
No variables, globs, or command substitutions are expanded; shell operators
require an explicitly invoked shell. An optional `command_line` draft field
preserves exact input, including unfinished quotes, across dismissal and relaunch.
Incomplete parsing blocks definition creation and execution, without blocking
draft saving. Published definitions still store only executable and arguments.

An omitted name is generated on creation as `executable-folder-word`, using the
executable and working-directory basenames plus a random word. An unspecified
or root working directory uses `default`. Generated names respect the 64-byte
limit and are stored with the definition; explicit names remain unchanged.
