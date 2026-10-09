# Proposal 0009: Published protocol contracts and negotiation

- Status: Implemented
- Created: 2026-10-09
- Implementation PR: [#68](https://github.com/tokn-ai/ctl/pull/68)

## Summary

Every named protocol advertises an explicit set of published contracts and
negotiates a shared contract before operations. Product releases, internal
protocol builds, published contract versions, and storage schemas are separate.
This extends the versioned transport boundaries in Proposals 0001–0006.

## Motivation

A matching product release or major version does not prove that a client can
send a particular operation. Running daemons, installed replacements, and
clients from different builds need an explicit compatibility contract.

## Design

Offers contain the internal build, latest published version, and supported
versions. The server selects the highest explicitly implemented shared version;
the client validates it. Advertised capability and selected behavior remain
separate in diagnostics. No intersection fails before the requested operation.

The first published contracts include negotiation. Earlier integer-only
formats were unpublished and have no implied compatibility. Each implementation
retains every earlier published contract in its major, including codecs and
behavior adapters. New operations and changed guarantees require negotiation
gates rather than merely checking the running binary's latest version.

The minor advances once when opening a new release cycle; subsequent protocol
changes advance the patch/build. Release freezes that contract without another
bump. An announced breaking major can drop earlier-major support. Storage
migration is independent.

Identified SSH channels use the stable `ctl-ssh-identity` marker. Identity
negotiation precedes companion startup, then the agent reports the account-owned
UUID and relays the chosen fixed service. The service negotiates its own
contract. Broker discovery uses a major-keyed endpoint so compatible development
builds can find the same owner.

## Invariants

1. Compatibility is an explicit intersection, never inferred from version ranges.
2. Every advertised contract has an implementation and compatibility coverage.
3. Clients gate operations by the selected contract, not the latest advertisement.
4. Same-major evolution retains all earlier published contracts.
5. Binary verification, product versions, and storage versions do not imply wire compatibility.
6. Identity negotiation does not replace OpenSSH authentication.

## Protocol impact

PR #68 establishes the first published contracts for `ctmux` (1.0.13/build 13),
`ctmux_control` (1.0.1/build 1), `ctld` (1.0.12/build 12),
`ctld_lifecycle` and `ctld_helper` (1.0.1/build 1), `task` (1.0.4/build 4),
`task_control` (1.0.2/build 2), `ctl_identity` (1.0.3/build 3),
and `ctl_maintenance` (1.0.2/build 2). Previous formats were unpublished
integer-only development protocols; this is not a compatibility promise for them.
Subsequent proposals record their own contract additions. Product release
versions are unchanged by this policy.

## Out of scope

Automatic daemon replacement, translating arbitrary historical development
formats, and changing storage schemas solely because a wire build changes.

## Unresolved questions

None for the implemented negotiation policy.

## Detailed specifications

- [Protocol versioning and release mapping](../protocol-versioning.md)
- [SSH framing and identity](../ctl-protocol.md)
- [ctmux wire protocol](../ctmux-protocol.md)
