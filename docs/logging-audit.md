# Logging and audit history

`ctld` records connection attempts and explicit disconnects, saved password and
passphrase reads/writes, and credential inventory/removal requests. Diagnostic
history also includes daemon startup/shutdown, proxy connections, and helper I/O.
`ctmuxd` also records session creation/termination/merging, pane splitting,
promotion, kill requests and process exits, view edits/zoom, and attachment
creation/resume/suspension/detach/expiry. Pane, divider, and canvas resize operations
and lease acquisition/release are debug diagnostics; transport lifetimes are trace
diagnostics. Session/pane events are not added to audit.

This covers the CLI and desktop when they use these daemon versions. Rebuild and
restart older running daemons to enable recording. Remote ctmuxd records on the
remote machine in that SSH user's home; `ctl logs` reads only the local machine.

```sh
ctl logs
ctl logs --failed --limit 50
ctl logs --level warn
ctl audit
ctl audit --json --limit 1000
ctl audit --path
ctl logs --run <full-run-uuid>
ctl audit --run <full-run-uuid>
```

These commands read local history without starting a daemon or connecting to a
host. They reject remote target options. The default is the newest 100 matching
records, displayed oldest first. Audit uses committed append order; logs use
timestamp order. Run UUID breaks ties between runs, and append order is retained
within a run for equal timestamps. `--limit` accepts 1–10000. `--failed` selects
failed and interrupted outcomes before applying the limit. Tables show UTC time,
a short run ID, severity, component, short session/pane IDs, event, result,
a short subject hash, duration, and a human-readable message with a fixed error classification.
JSON includes full run IDs, event/operation IDs, process IDs, timestamps, and
completeness information.

## Contents and privacy

The local record schema is version 4, adding human-readable messages to diagnostic lines.
Version 3 added severity, component, and typed context
(session, pane, attachment IDs, exit code, and lease kind). Version 2 audit payloads
remain readable with default context, and version 2/3 log lines remain readable.
Each process generates one `run_id` UUID;
PID reuse cannot collide with a different run. All helper invocations and daemon
runs get separate IDs. Each operation that spans work records `started` and a terminal
outcome with the same `operation_id`; each record has its own `event_id`. Terminal
outcomes are `succeeded`, `missing`, `failed`, or `interrupted`. Cancellation that
drops an operation records interruption. A hard process kill can leave a start
without an outcome. A successful helper I/O record means its response was written;
the separate credential operation records whether the requested action succeeded.

Only typed, allowlisted metadata is recorded. Subject IDs are SHA-256 hashes of
connection identities or credential identifiers. They support correlation without
putting hostnames, accounts, key paths, passwords, passphrases, private key bytes,
raw requests, or raw error messages in history. Hashes are deterministic, not
anonymization against guessing. Error details are fixed codes and, where available,
a numeric OS status. Only generated session, pane, and public attachment UUIDs
are recorded: session names, working directories, commands, terminal input/output,
and secret reconnect attachment tokens are excluded.

## Diagnostic levels and format

The levels are `trace`, `debug`, `info`, `warn`, and `error`. Daemons save `info`
and above by default. Set `CTL_LOG_LEVEL` before starting a daemon to change its
minimum severity; invalid values warn and fall back to `info`. For example:

```sh
CTL_LOG_LEVEL=debug ctmuxd --socket /path/to/private/runtime/ctmux.sock
ctl logs --level warn
```

Changing the environment does not reconfigure an already running daemon.
`--level` filters stored records before applying the display limit; it does not
turn on additional recording. Diagnostic filtering never suppresses audit events.
Expected rejected view operations are warnings, PTY/spawn/I/O failures are errors,
nonzero pane exits and lost/expired attachments are warnings. Ordinary lease
contention stays at debug level. A terminal failure can therefore be saved even
when its lower-severity start record was filtered out.

Diagnostics are human-readable `.log` files. Every line starts with a UTC RFC 3339
timestamp, uppercase severity, component, event, and outcome, followed by a quoted message and fixed
key/value metadata. An example prefix is:

```text
2026-10-09T08:15:30.123Z INFO ctmuxd pane_split succeeded  message="Split pane: completed"
```

The full line also carries IDs, duration, exit status/lease kind where applicable,
and fixed error codes. `ctl logs --json` reconstructs typed records from these
lines. Partial/corrupt lines are reported as omitted instead of silently accepted.
Diagnostic writes occur outside the session registry and PTY operation locks.

## Call-site policy

- Use a correlated `Operation` for work with a duration: connection, credential
  access, session creation, resize, and transport lifetime. The owning boundary
  records its result once; callers do not print a second raw error.
- Use `diagnostic_event` for an observation that has already happened: pane exit,
  attachment suspension/detachment/expiry, and lease state. It writes one record,
  without an artificial `started` event. These observations are not audit events.
- Messages are centrally derived from event, outcome, fixed error code, and safe
  numeric context. No call site can attach a free-form request/error message.
  Files carry a JSON-quoted `message` near the beginning of the line; `ctl logs`
  tables and `--json` derive the same description, including for legacy records.
  Wording may change without invalidating older lines; typed fields are canonical.
- Foreground ctld/ctmuxd diagnostics mirror the same timestamped renderer to a
  stderr, including redirected stderr, respecting `CTL_LOG_LEVEL`. Detached
  daemons save files without mirroring; helper entry points mirror only to a terminal.
  Persistent diagnostics remain available when auto-launch discards stderr.
  Recorder failures use a minimal stderr fallback to avoid recursive logging.
- CLI prompts, progress, command results, and final user-facing errors keep their
  terminal output. Agent stdout remains a protocol channel. Frontend console
  errors, task-daemon stderr, and container output remain separate producers;
  they are not automatically captured by this recorder.

Invalid `CTL_LOG_LEVEL` and failed VPN container monitoring are now persisted
warnings. Startup failures retain typed classifications (bind, runtime directory,
already-running endpoint, and so on) and numeric OS errors where available;
transport errors distinguish timeout, framing/I/O, journal, and worker failures.

## Storage and failures

Local ctld and ctmuxd processes share `~/.tokn/ctl/history`. On Unix,
new directories use mode 0700 and files 0600. Unsafe ownership/permissions,
symlinks, hardlinked files, and non-regular files are refused, including SQLite
sidecars. SQLite is bundled at build time; users need no SQLite installation.

```text
~/.tokn/ctl/history/
  audit.sqlite3
  logs/
    <run_id>.lock
    <run_id>.log
    <run_id>.1.log
    ...
```

All ctld processes share `audit.sqlite3`. Each event is inserted in a SQLite
transaction, with a unique event ID and indexed time, run, subject, and outcome.
Database schema version 1 is identified by SQLite `application_id` and
`user_version`; it is separate from local record schema version 4 and named wire
protocols. Unsupported schemas and corrupt databases are refused without resetting
or overwriting them. Full synchronous commits and rollback journaling make
successful inserts durable and allow CLI queries without creating WAL sidecars.
Concurrent writers wait up to 250 ms for SQLite locks. If a killed writer leaves
a hot rollback journal, the next writer recovers it. A read-only CLI query can
report recovery-required until that happens, rather than changing the database.
Audit has no automatic
age/size deletion in this version; its disk use grows with recorded activity.

Diagnostic files are independent for each process run. A run retains four
segments of up to 5 MiB each (20 MiB per run). Its newest segment has no number;
older diagnostic segments use `.1.log`, `.2.log`, and `.3.log`. Rotation only discards
that run's oldest segment. Threads and readers coordinate with that run's file
lock, with a bounded 250 ms wait. One run cannot rotate or block another run's
writes. Completed runs are retained; there is no automatic cleanup across runs.
`ctl logs` combines their newest matching records; `--run` selects one full UUID
from `--json`. Audit supports the same filter.

Every diagnostic append is flushed to disk. An interrupted partial line is
separated from the next append. Readers omit malformed/unsupported records and
return `complete: false` with a warning. Completeness describes inspected retained
records, not records rotated away, failed writes, or operations killed before
recording. Legacy audit JSONL files are left untouched and explicitly reported
as unimported. Legacy diagnostic files are also preserved and reported as omitted;
this version does not migrate the earlier unreleased format.

Recording failures do not fail connection or credential operations: ctld warns
on stderr, suppressing consecutive recording failures. A launcher that redirects
daemon stderr must retain it to make warnings visible; this implementation does
not add desktop alerts. ctmuxd uses the same warning policy.

History is best-effort local troubleshooting evidence, not a tamper-proof security
ledger. The owning user can edit or remove it. `ctl passwords clear` removes saved
credentials, not history. No named wire protocol changes are required.
