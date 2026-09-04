# Shared local task definitions

The CLI and desktop use the same `task-store` Rust repository for saved
definitions. Direct command saves and catalog operations do not start taskd.
Registered tasks and their active/latest run records remain owned by taskd;
saving a definition does not modify an existing registered task or process.

## Locations and scopes

| Scope | File |
| --- | --- |
| Project | `<project-root>/.ctl/tasks.json` |
| Global on macOS | `~/Library/Application Support/ctl/tasks.json` |
| Global on Linux | `$XDG_CONFIG_HOME/ctl/tasks.json`, defaulting to `~/.config/ctl/tasks.json` |
| Global on Windows | `%APPDATA%\ctl\tasks.json` |

CLI discovery walks upward from the current directory and stops at the nearest
`.ctl/tasks.json` or `.git` file/directory. Git worktrees and nested repositories
therefore have their own project scopes. `--project PATH` selects an explicit
existing project directory; `--global` selects only the global catalog.

Unqualified CLI reads look in the discovered project first, then global. Writes
and removals use only the selected scope, or the discovered project when present,
otherwise global. A failed project lookup during removal never deletes a global
definition of the same name. Names must be unique within each catalog.

The desktop uses explicit Global/Project selection in Tasks. Its Project input
requires an absolute directory and resolves aliases to the canonical project
path. CLI edits appear on refresh or when the app regains focus. Open drafts
retain their original scope and base revision even if the selected catalog changes.

Project catalogs may be version controlled. Exclude `.ctl/.tasks.json.lock` and
`.ctl/.tasks-*.tmp` from version control; these coordinate writers and temporary
replacement files rather than defining tasks.

## CLI

```sh
# Save in this project, capturing the caller's current directory.
ctl task save build -- cargo build
ctl task definitions list
ctl task definitions show build

# Register an independently named managed task from that saved command.
ctl task create app-build --from-definition build --start

# Copy an active or latest retained run's definition snapshot.
ctl task save previous-build --from-run RUN_ID

# Edit or remove only the version you inspected with definitions show.
ctl task save build --definition-id DEFINITION_ID --expected-revision REVISION -- cargo test
ctl task definitions remove build --expected-revision REVISION
```

Add `--global` or `--project PATH` to select a catalog explicitly. Updates require
both a stable definition ID and the expected revision. A new save fails on an
existing name or ID rather than replacing it. Renaming keeps the definition ID.

`save --from-run` connects to local taskd, starting the daemon if needed to read
retained snapshots. It does not start a new task. It uses the stored snapshot
belonging to the selected run, even
if the registered task has since been edited. Only active/latest retained runs
are available through the current task protocol. An unknown run or missing
snapshot is an error. Omitted or relative directories in that snapshot preserve
their existing host-home semantics; the snapshot cannot recover execution
context that was never recorded.

`create --from-definition` copies the saved command, mode, and directory into a
new registered task using the requested instance name. Later definition edits
do not update that task. Existing registered-task names remain unique per taskd,
and existing create/start behavior remains intact.

These catalog commands and saved-definition creation are local-only. Supplying
`--host` fails before touching a local file or opening a daemon connection.
Legacy remote create/start commands retain their current behavior.

## Format and save conflicts

Each file is a versioned JSON document with `schema_version: 1` and a
`definitions` array. Every entry has `definition_id`, `revision`, and
`definition`. The definition contains `name`, `program`, `arguments`,
`working_directory`, and `execution_mode`, matching the current task protocol.

Revisions are SHA-256 digests of the serialized definition values. Reads derive
them from the actual content instead of trusting the stored revision string.
Hand edits therefore invalidate stale saves even if the editor leaves that
string unchanged. Cosmetic JSON formatting changes do not change the revision.

Writers lock a separate sibling lock file, reread and validate the current
document, compare the expected definition revision, then publish a synced
temporary file by atomic replacement. Files are bounded to 4 MiB. Corrupt JSON,
unsupported versions/fields, duplicate IDs/names, and non-regular target files
are rejected without replacing their contents. Saves to different definitions
preserve each other's changes; stale edits to the same definition conflict.

Desktop conflicts retain the draft. Reloading the saved definition is explicit;
refreshing the catalog never silently replaces the draft's base revision.

## Desktop migration

Workspace schema 3 stores presentation, definition source selection, drafts,
and task references. Saved definitions are no longer embedded in workspace
writes. This prevents an old workspace snapshot from overwriting a CLI edit.

On first load, the native backend creates a synced, atomically published
`workspace-v2.backup.json` before importing legacy definitions into the global
catalog. Imports preserve IDs and command/directory values, and are idempotent
across retries. Conflicting IDs or names stop the migration without overwriting
either definition. References whose applied revision matches the legacy saved
revision are updated to the imported content revision. Live tasks and sessions
are never contacted during migration.

After import succeeds, the workspace is committed as schema 3 with external
references. A failed final workspace write can be retried without duplicating
definitions. Legacy drafts with unknown base revisions require an explicit
review/reload before overwriting a saved definition.

## Design boundary

This implements the shared definition-storage portion of
[Proposal 0007](proposals/0007-local-task-workflows.md). Independent one-off runs,
invocation-directory policies, scheduling, and automatic restart remain proposed.
Direct CLI saves currently capture a fixed caller directory, just as legacy
create does; they are not yet dynamic aliases. Ctrl+Z / `task bg` adoption remains
deferred for separate investigation.
