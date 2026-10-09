# Proposal 0015: Local diagnostic logs and credential audit history

- Status: Implemented
- Created: 2026-10-09
- Updated: 2026-10-09
- Implementation PRs: [#93](https://github.com/tokn-ai/ctl/pull/93),
  [#95](https://github.com/tokn-ai/ctl/pull/95)

## Summary

Ctld and ctmuxd record typed local diagnostics; ctld also records durable
connection and credential audit events. `ctl logs` and `ctl audit` query local
history without starting daemons or connecting to hosts.

## Motivation

Failures in detached daemons need inspectable evidence. Credential access and
connection operations need correlated outcomes without recording secrets,
terminal contents, or arbitrary request/error strings.

## Design

Each process run has a UUID. Duration-bearing operations record a start and
terminal outcome under one operation ID; each event also has a unique event ID.
Observations such as pane exits or attachment expiry record one diagnostic
event. Cancellation can record interruption; hard termination may leave an
operation without an outcome.

Allowlisted metadata and centrally derived messages exclude key paths, commands,
cwd, terminal bytes, secrets, and reconnect tokens. Connection/disconnect records
now include the submitted SSH endpoint (destination, hostname, account, port)
in local history; legacy records remain hash-only. Ensure/reuse requests are
debug diagnostics, while actual connection establishment is recorded at info. Subject
hashes support correlation but are not anonymization against guessing. Generated
session/pane/public attachment IDs and numeric statuses provide safe context.
Session/pane activity is diagnostic only, not credential audit.

Diagnostics have trace/debug/info/warn/error levels, defaulting to info and above.
`CTL_LOG_LEVEL` configures a process at startup; CLI filters only select stored
records. Audit is independent of diagnostic severity. Foreground daemons mirror
the same safe renderer to stderr; agent stdout remains a protocol channel.

Each run has human-readable log files with four bounded 5 MiB segments. Audit
uses shared SQLite transactions with unique event IDs, indexed query fields,
full synchronous commits, and rollback journaling. Files live in private local
history storage with bounded lock waits and unsafe-path checks. SQLite schema 1
and record schema 5 are distinct; supported older payloads remain readable.
Legacy files are preserved and reported rather than silently imported.

Queries report omitted malformed records and incomplete inspection. Read-only
audit queries do not recover a hot journal; recovery may require the next writer.
Recording failures warn without failing the connection or credential operation.
Completed runs and audit records have no automatic cross-run cleanup. Remote
daemons record on their own machine; the local CLI does not aggregate that data.

## Invariants

1. History contains typed safe metadata, never free-form secret-bearing payloads.
2. Diagnostic filtering does not suppress audit.
3. Run/operation/event IDs distinguish process reuse and correlated outcomes.
4. Recording failure does not break product operations or recursively log itself.
5. Corrupt/unsupported history is preserved and reported, never reset silently.
6. Completeness describes inspected retained records, not all past activity.

## Protocol impact

Protocol changes: none. Local record schema 5 and SQLite database schema 1 are
storage formats, independent of product releases and named wire contracts.
The leveled recorder replaces earlier unreleased storage without claiming to
migrate every legacy file.

## Out of scope

A tamper-proof ledger, remote aggregation, automatic global retention cleanup,
terminal recording, frontend/container/taskd stderr capture, and desktop alerts
for recorder failures.

## Unresolved questions

None for the implemented local recording boundary. Cross-run cleanup and
remote aggregation require separate designs if pursued.

## Detailed specifications

- [Logging, privacy, retention, and failure behavior](../logging-audit.md)
- [Recorder implementation](../../ctl-core/src/observability.rs)
- [Protocol/storage version separation](../protocol-versioning.md)
