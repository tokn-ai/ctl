# Proposal 0017: Manage persistent sessions from ctl shell

- Status: Implemented
- Created: 2026-10-11
- Updated: 2026-10-12

## Summary

Make the existing shell session picker a session manager, and add
`ctl shell --resume`, `ctmux --resume`, and `ctl ctmux --resume` to open that
manager before attaching. Users can find,
open, create, rename, and explicitly terminate sessions on the selected host.
This extends [Proposal 0014](0014-shared-panes-tui.md) and uses the shared Rust
client API from [Proposal 0016](0016-shared-actions.md).

## Motivation

Persistent shells outlive their viewers. Repeated `ctl shell` invocations create
new sessions, while the current picker only shows names and opens an existing
session. Users need a clear way to return to earlier work and manage it from
inside the terminal UI.

## Design

Keep ordinary `ctl shell` startup creating a session. The existing
`--session NAME` option keeps its attach-or-create behavior, and `--plain`
keeps opening an ordinary SSH or local shell.

The resume entry points list running sessions on the selected endpoint. When
sessions exist, it opens the manager without creating a session, attaching to
one, or taking input or resize leases. When no sessions exist, it creates and
attaches to one new shell. `ctl shell --resume` is exclusive with `--session`, `--plain`,
and `--cwd`; the latter describes the initial directory of a newly requested
session. Ctmux resume is a shared typed startup action, also spelled `resume`,
and is exclusive with another ctmux subcommand. Standalone endpoint and prefix
options still apply.

Inside a shell, Ctrl+B s/w and `:list-sessions` open the same manager. It shows
session names, stable short IDs, status, and the current-session marker. Enter
opens the selected session; creation, renaming, and whole-session termination
are available in the manager. Leaving a new session's name empty and pressing
Enter requests a unique daemon-generated name, such as `session-1`. The shared
Rust session client treats missing, empty, and whitespace-only creation names
as omitted across CLI, desktop, and TUI callers. Other names remain literal
and use the daemon's existing session-name validation; rename still requires
a nonempty valid name. Cancellation returns to the current shell, or exits the unattached
manager opened by `--resume`.

Termination confirms the chosen name and ID and explains that all its panes
and viewers are affected. Killing the currently viewed session returns to the
manager rather than creating replacement work. Pane termination and client
detach remain separate actions. Read-only viewers may navigate and attach;
creation, rename, and termination remain unavailable.

The local command prompt adds `rename-session [-t NAME_OR_ID] NAME` and
`kill-session [-t NAME_OR_ID]`. The latter opens the same confirmation.
The canonical ctmux CLI exposes `rename-session -t NAME_OR_ID NAME`, also
available through `ctl ctmux`. Parsers and UI adapters call the shared Rust
`SessionClient`; they do not invoke another CLI process or a desktop bridge.

Renaming changes the root name in ctmuxd while preserving its session, view,
and terminal IDs, processes, history, layout, and leases. List results and
attached view snapshots carry the authoritative new name. A name change advances
the view revision so delayed snapshots cannot restore the previous name; the
layout and geometry stay unchanged. Task-owned roots retain task lifecycle
ownership and reject independent renaming.

## Invariants

1. Discovery and `--resume` with existing sessions never create persistent work.
2. Manager actions remain scoped to the invocation's selected host and endpoint.
3. Confirmation and name editing pin the session ID; refresh cannot redirect a
   mutation to the next row or to a newly created session with the same name.
4. Failed or cancelled navigation preserves the current attachment.
5. Rename preserves every identity and process; termination ends every pane
   in the chosen root for every attached viewer.
6. Read-only clients cannot mutate sessions through manager or prompt controls.
7. Mutations are not automatically retried after an uncertain response.
8. Input/layout ownership and quiet background authentication retain their
   existing policies.

## Protocol impact

`ctmux` advances from **1.1.19/build 19** to **1.1.20/build 20**. The additive
one-shot `rename_session { session, name }` operation returns
`session_renamed { session }` with authoritative session information. Rename
requires negotiated 1.1.20, with rejection before sending to older daemons.
Updated daemons retain 1.0.13 and all published 1.1.14–1.1.19 contracts.
Existing list, creation, attachment, and termination messages remain unchanged.
Attached name updates reuse the existing `view_snapshot` shape.

`ctmux_control`, `ctld`, `ctld_helper`, and all other named contracts keep
their versions and internal builds. Product release and storage schema
versions are unchanged. Client session-management controls and `--resume`
have no independent wire contract. Blank creation names reuse the existing
omitted-name request and daemon allocator; they do not alter wire validation.

## Out of scope

Cross-host session discovery, automatic selection of the newest session,
automatic termination, client lease takeover, task renaming, session migration,
and surviving a ctmuxd restart.

## Unresolved questions

None. The user chose management inside the shell, a picker for `--resume`,
and authoritative session renaming in this change.

## Detailed specifications

- [TUI controls](../../apps/tui/README.md)
- [ctmux protocol](../ctmux-protocol.md)
- [Protocol versioning](../protocol-versioning.md)
- [Shared action API](../action-api.md)
