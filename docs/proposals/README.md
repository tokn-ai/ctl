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
implemented boundary require a new proposal that supersedes the old one;
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
| [0001](0001-rmux.md) | Persistent terminal sessions with rmux | Implemented |
| [0002](0002-ctl.md) | Local and SSH control routing with ctl | Implemented |
| [0003](0003-task-system.md) | Managed tasks in ctl | Proposed |
