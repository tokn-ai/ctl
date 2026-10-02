# Reusable local task definitions

Read this reference for `ctl task save`, `ctl task definitions`, creation from
a saved definition, or copying a retained run snapshot.

A saved definition is a local recipe: name, program, arguments, working
directory, and execution mode. It is separate from a registered task owned by
ctl-taskd. Direct saves and catalog operations do not connect to ctl-taskd or start a
process. Creating from a definition copies its values into a new registration;
later catalog changes do not update that task. These operations are local-only:
`--host` is rejected, including for `create --from-definition` and
`save --from-run`.

## Scope and discovery

| Scope | Catalog path |
| --- | --- |
| Project | `<project-root>/.ctl/tasks.json` |
| Global, all platforms | `~/.tokn/ctl/tasks.json` |

With no explicit scope, CLI discovery walks upward from the caller's cwd to
the nearest `.ctl/tasks.json` or `.git` file/directory. Worktrees and nested
repositories consequently have separate project scopes. `--project PATH`
selects an existing project directory; `--global` selects only the global
catalog. The two flags are mutually exclusive.

Unqualified reads search the discovered project, then global. Saves and
removals use only the discovered project when present, otherwise global.
Removing a missing project entry never falls back to deleting a global entry.
Choose an explicit scope when a read's fallback result is the intended write
target. Definition names are unique within each catalog; selectors accept a
name or stable definition ID.

## Save, inspect, and register

```sh
ctl task save build --cwd /srv/api -- cargo build
ctl task definitions list
ctl task definitions show build
ctl task create api-build --from-definition build --start

ctl task save console --global --cwd /srv/api --mode interactive -- bash
ctl task create api-console --global --from-definition console --start
```

A direct save defaults to background mode and captures the caller's cwd;
relative `--cwd` resolves against that cwd. The saved directory is fixed, not
a dynamic alias for whichever directory invokes the definition later.
`--project` changes catalog selection, not the command's working directory.

Save and `definitions show` emit pretty JSON without a `--json` flag. The
JSON contains `definition_id`, `revision`, and `definition`. List emits a
table with scope, ID, revision, and name. Use the inspected ID and revision
for subsequent changes; do not invent revisions or assume a repeated save
overwrites an existing name.

`create INSTANCE --from-definition SELECTOR` retains the copied program,
arguments, cwd, and mode while using INSTANCE as the registered task's name.
Its name must be unique in ctl-taskd. `--from-definition` conflicts with an inline
command, `--cwd`, and `--mode`; specify the desired values in the saved recipe.
Scope flags on `task create` require `--from-definition`.

## Change or remove a definition

```sh
ctl task definitions show build --project /srv/api
ctl task save build --project /srv/api --definition-id DEFINITION_ID --expected-revision REVISION --cwd /srv/api -- cargo test
ctl task definitions remove build --project /srv/api --expected-revision REVISION
```

Replace the placeholders with the latest inspected JSON values. Updating
requires both `--definition-id` and `--expected-revision`; renaming keeps that
stable ID. A save supplies the whole replacement recipe, so repeat the
desired cwd and mode instead of assuming omitted values retain the previous
settings. On a revision conflict, reread the selected definition and reconcile
the requested change before retrying. A fresh save fails on an existing name
or ID instead of replacing it.

Prefer CLI catalog operations to hand-editing the file: they validate its
schema and use locks, content revisions, and atomic replacement. Project
catalogs can be version controlled; exclude `.ctl/.tasks.json.lock` and
`.ctl/.tasks-*.tmp`, which are writer coordination files. Catalog removal
does not stop or remove an already registered task.

## Copy a retained run

```sh
ctl task save previous-build --from-run RUN_ID --project /srv/api
```

This connects to local ctl-taskd, starting it if necessary to read metadata,
without starting a new run. Supply an actual run ID; a task ID is different,
and current `task show` output does not expose run IDs. Only active/latest
retained runs are available, and a missing definition snapshot is an error.
Do not claim to recover arbitrary run history.

The copied snapshot preserves the run's command, mode, and recorded directory
even if its registration has since changed. `--from-run` conflicts with an
inline command, `--cwd`, and `--mode`. The supplied save name replaces the
snapshot's name. Omitted or relative directories in older snapshots keep
their target-home semantics; the snapshot cannot reconstruct cwd context
that was never recorded. Inspect the saved JSON before registering it when
the directory matters.
