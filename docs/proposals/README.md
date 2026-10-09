# Design proposals

Design proposals record the intent and major boundaries of repository features.
They explain why a feature exists and what the implementation must preserve.
Detailed wire formats and operational procedures belong in separate documents
linked from the proposal.

## Status

- **Proposed**: under discussion and not approved for implementation.
- **Accepted**: approved for implementation.
- **Implemented**: the accepted design is present in the repository.
- **Withdrawn**: no longer planned.
- **Superseded**: replaced by another numbered proposal.

A proposal keeps its number when its status changes. Material changes to an
implemented boundary require a new proposal that extends or supersedes the old one;
clarifications and links may update the existing document.

## Process

1. Copy [`0000-template.md`](0000-template.md) to the next unused four-digit
   number and give it a descriptive filename.
2. Describe the motivation, boundaries, user experience, invariants, and
   unresolved questions.
3. Keep the status **Proposed** while decisions remain open.
4. Change the status when the design is accepted, implemented, withdrawn, or
   superseded.
5. Update this index.

## Index

| Number | Title | Status |
| --- | --- | --- |
| [0001](0001-ctmux.md) | Persistent terminal sessions with ctmux | Implemented |
| [0002](0002-ctl.md) | Local and SSH control routing with ctl | Implemented |
| [0003](0003-task-system.md) | Managed tasks in ctl | Proposed |
| [0004](0004-windows-ssh.md) | Windows SSH gateways | Implemented |
| [0005](0005-desktop-tasks.md) | Tasks in the desktop workspace | Implemented |
| [0006](0006-remote-tasks.md) | Explicit task routing over SSH | Implemented |
| [0007](0007-local-task-workflows.md) | Local task definitions, runs, and schedules | Proposed |
| [0008](0008-connection-cli.md) | Native connection commands and OpenSSH compatibility | Implemented |
| [0009](0009-protocol-contracts.md) | Published protocol contracts and negotiation | Implemented |
| [0010](0010-vpn-host-routes.md) | Shared VPN profiles and linked host routes | Implemented |
| [0011](0011-credentials-reconnect.md) | Credential inventory and scoped reconnect approval | Implemented |
| [0012](0012-component-maintenance.md) | Verified component bundles and explicit maintenance | Implemented |
| [0013](0013-terminal-history.md) | Paged terminal history without blocking live output | Implemented |
| [0014](0014-shared-panes-tui.md) | Shared pane geometry and terminal client controls | Implemented |
| [0015](0015-logging-audit.md) | Local diagnostic logs and credential audit history | Implemented |

## Current design and implementation history

Proposals 0009–0015 record implemented boundaries from recent merged PRs, grouped
by design area rather than one document per PR. Their creation dates identify
when the design record was written; linked PRs identify the implementation
history. Build tooling, test infrastructure, and routine fixes do not need
separate product proposals unless they change a design boundary.

Original proposals retain their scope and link to extensions. Historical schema
and command plans are labeled as such; linked detailed specifications describe
the current wire and storage formats. A new proposal can extend an implemented
boundary without replacing every invariant of its predecessor. Use Superseded
when the predecessor's design itself is replaced.

Proposal 0003 remains Proposed because automatic restart and durable full
history are unfinished, despite its implemented task core. Proposal 0007 remains
Proposed because independent invocation, context policies, and scheduling are
unfinished, despite implemented shared definition catalogs. Proposal 0005's
local desktop integration is implemented; remote task UI remains outside it.

Every new proposal should state its protocol impact, distinguishing named
contract versions and internal builds from product releases and storage schemas.
For retrospective records, describe the implementation PR's impact; writing the
record does not itself change a protocol. Link source PRs and the authoritative
detailed specifications rather than duplicating every wire field.
