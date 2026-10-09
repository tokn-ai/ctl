# Proposal 0010: Shared VPN profiles and linked host routes

- Status: Implemented
- Created: 2026-10-09
- Implementation PRs: [#58](https://github.com/tokn-ai/ctl/pull/58),
  [#64](https://github.com/tokn-ai/ctl/pull/64),
  [#65](https://github.com/tokn-ai/ctl/pull/65),
  [#70](https://github.com/tokn-ai/ctl/pull/70),
  [#72](https://github.com/tokn-ai/ctl/pull/72),
  [#76](https://github.com/tokn-ai/ctl/pull/76)

## Summary

CLI and desktop share saved VPN profiles and host methods. A route can reuse
another saved host method or a VPN running on an SSH host. Runtime connection
ownership stays in ctld, while catalog entries describe reusable configuration.
This extends Proposal 0008 without widening the ctmux/task service relay.

## Motivation

Private endpoints often require several SSH/VPN hops. Duplicating endpoint and
route configuration makes updates inconsistent, while tying a VPN container to
one client prevents safe reuse and recovery.

## Design

Saved profiles and runtime VPN inventory are distinct. Explicit actions start,
stop, reconnect, or remove configuration; merely finding a saved profile does
not establish that it is running or signed in. CLI workflows expose the same
profiles used by the desktop.

Linked route steps bind stable host and method IDs. Resolution expands the
referenced method's current route and endpoint, rejects missing references and
cycles, and limits expansion to eight hops. Referenced hosts/methods cannot be
removed until dependent links are resolved. New methods default to the preferred
method's SSH endpoint, but remain independently editable. Existing sessions
retain their resolved transport snapshots.

Remote VPN operations use the separate fixed `ctl-agent vpn` command. After
contract negotiation and account identity verification, bounded requests allow
list, start, exact-ID stop, or a TCP connection through a saved VPN. Successful
connect changes to a byte stream; DNS resolution happens through the remote
VPN's SOCKS listener. The agent reaches only its account's fixed ctld endpoint.
List, stop, and connect do not start ctld or a VPN; explicit start may do so.

Shared containers use independent broker heartbeats rather than a single
client's lifetime. Recovery reopens remote streams through the selected route;
a stale endpoint is resolved again. Transport loss does not by itself prove
that the remote VPN has stopped. Existing authenticated SSH hops can be reused
without silently replacing the route with a direct connection.

## Invariants

1. Saved configuration, runtime inventory, reachability, and authentication are separate evidence.
2. Routes never fall back silently to direct access or a different private master.
3. Linked routes reject cycles, dangling references, and excessive expansion.
4. Identity is verified before sending remote VPN settings or credentials.
5. Remote control exposes a bounded VPN operation set, not arbitrary broker requests.
6. One client's disconnect does not claim ownership of every shared container.

## Protocol impact

PR #70 adds `ctl_remote_vpn` 1.0.1 (build 1), with no previous published
contract for that channel. Remote route steps advance `ctld` from 1.0.12
(build 12) to 1.1.13 (build 13) and `ctld_helper` from 1.0.1 (build 1)
to 1.1.2 (build 2), retaining their initial contracts. Old broker/helper
contracts retain local VPN and original SSH behavior; new route operations
require the corresponding selected contract. Catalog and container behavior
changes do not introduce additional named contracts.

## Out of scope

Arbitrary remote broker endpoints, machine provisioning, automatic trust-pin
replacement, and interpreting passive reachability as successful authentication.

## Unresolved questions

None for the implemented route and VPN boundary.

## Detailed specifications

- [SSH and remote VPN transport](../ctl-protocol.md#remote-vpn-requests)
- [Host catalog and linked routes](../ctmux-workspace.md)
- [Connection evidence and recovery](../connection-state.md)
- [Protocol versions](../protocol-versioning.md)
