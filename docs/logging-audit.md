# Logging and audit history

`ctld` records connection attempts and explicit disconnects, saved password and
passphrase reads/writes, and credential inventory/removal requests. Diagnostic
history also includes daemon startup/shutdown, proxy connections, and helper I/O.
This covers the CLI and desktop when they use this version of `ctld`. Rebuild and
restart an older running daemon to enable recording.

```sh
ctl logs
ctl logs --failed --limit 50
ctl audit
ctl audit --json --limit 1000
ctl audit --path
```

These commands read local history without starting a daemon or connecting to a
host. They reject remote target options. The default is the newest 100 matching
records, displayed in append order; `--limit` accepts 1–10000. `--failed` selects
failed and interrupted outcomes before applying the limit. Tables show UTC time,
event, result, a short subject hash, duration, and a fixed error classification.
JSON includes full IDs, process IDs, timestamps, and completeness information.

## Contents and privacy

The JSON Lines schema is version 1. Each operation records `started` and a terminal
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
a numeric OS status.

## Storage and failures

Both signed desktop and standalone helpers share `~/.tokn/ctl/history`. On Unix,
new directories use mode 0700 and files 0600. Unsafe ownership/permissions,
symlinks, hardlinked files, and non-regular files are refused. Processes coordinate
appends and rotation with a file lock, with a bounded 250 ms wait per append.
Each append is flushed to disk. Logging failures do not fail the connection or
credential operation: ctld warns on stderr, suppressing consecutive recording
failures. A launcher that redirects daemon stderr must retain it to make
these warnings visible; this first implementation does not add desktop alerts.

`logs.jsonl` retains four segments of up to 5 MiB each (20 MiB total);
`audit.jsonl` retains sixteen (80 MiB total). The newest segment has no number;
older segments use `.1.jsonl`, `.2.jsonl`, and so on. Rotation discards the oldest
segment. There is no time-based retention guarantee. A record interrupted during
writing is separated from the next append. Readers skip malformed/unsupported
records and return `complete: false` with a warning. Completeness describes the
segments scanned for the requested limit, not history already rotated away.

History is best-effort local troubleshooting evidence, not a tamper-proof security
ledger. The owning user can edit or remove it. `ctl passwords clear` removes saved
credentials, not history. No named wire protocol changes are required.
