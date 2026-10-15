# Proposal 0012: Verified component bundles and explicit maintenance

- Status: Implemented
- Created: 2026-10-09
- Updated: 2026-10-15
- Implementation PRs: [#62](https://github.com/tokn-ai/ctl/pull/62),
  [#66](https://github.com/tokn-ai/ctl/pull/66),
  [#71](https://github.com/tokn-ai/ctl/pull/71),
  [#73](https://github.com/tokn-ai/ctl/pull/73),
  [#81](https://github.com/tokn-ai/ctl/pull/81),
  [#85](https://github.com/tokn-ai/ctl/pull/85),
  [#86](https://github.com/tokn-ai/ctl/pull/86),
  [#98](https://github.com/tokn-ai/ctl/pull/98),
  [#100](https://github.com/tokn-ai/ctl/pull/100)

## Summary

Inspect running owners separately from installed replacements. Import and
install verified complete builds locally or remotely, then reconnect or restart
through separate explicit actions. This extends Proposal 0002's original
maintenance exclusion with narrow commands, never an endpoint relay.

[Proposal 0018](0018-daemon-hotreload.md) investigates full daemon replacement
while preserving live work. It does not change this proposal's implemented,
destructive restart behavior or make current owners support hotreload.

## Motivation

A product version cannot identify a running development binary or prove wire
compatibility. Updating files must not unexpectedly stop sessions, and restarting
must target the inspected owner rather than a replacement that appeared later.

## Design

Complete bundles contain ctl-agent, ctld, ctmuxd, and ctl-taskd, with component
metadata, target, source, and checksums. Verification checks archived bytes,
manifest agreement, client compatibility, and companion contract compatibility.
Verified cached schema-2 bundles may come from another release or revision;
schema-1 reuse remains exact-source only. Foreign-target imports do not execute
those binaries.

Desktop About and CLI status distinguish running, installed, and last-observed
metadata. Desktop refresh reuses authenticated connections without opening a
service or installing components. CLI remote status may establish the selected
SSH/VPN route, then verifies identity and inspects companions without starting
those remote services. Missing automatically discovered replacements remain
visible as not installed; explicit invalid overrides remain errors.

Update chooses agent-only or a full bundle, using a compatible complete source
in either case. Agent-only installs record retained daemon locations rather than
pretending to contain a new full bundle. Batch updates preserve per-host outcomes
and permit retries for failed hosts. Selection and installation preserve existing
owners; reconnect applies a replacement agent to future channels.

Restart verifies the replacement, pins the owner, presents its impact, and
requires separate confirmation. Plans expire after 20 seconds. Dry-run performs
preparation without mutation; JSON output alone is not consent. Cooperative
shutdown and successor verification do not force-kill or retry uncertain results.
An absent owner is not started by restart. Remote restart supports ctmuxd only;
local taskd refuses restart while tasks are active. Ctmuxd restart ends all its
sessions, including interactive tasks; ctld restart interrupts clients and VPNs.

Signed macOS helper discovery is shared by ordinary connection setup and restart
preparation, respects explicit overrides, and prefers eligible local signed
builds during development. Passive status does not provision embedded helpers.
Checkout-scoped development helpers are copied into the existing immutable
managed development cache, then verified before launch without changing shared
selections. They remain available to running daemons and their child helpers
after Cargo output cleanup. A missing running helper blocks new connection
attempts before credential access; existing master reuse remains available until
an explicit restart applies the replacement.

## Invariants

1. Installed bytes, running owners, and product versions remain distinct facts.
2. Compatibility and integrity are independently verified before activation.
3. Installing or selecting a build does not restart an owner.
4. Restart confirmation applies only to the prepared owner and replacement.
5. Uncertain mutation is reported for inspection, never automatically repeated.
6. Remote inspection/restart is account-pinned and cannot choose arbitrary IPC endpoints.

## Protocol impact

Companion inspection advances `ctl_maintenance` from 1.0.2/build 2 to
1.1.3/build 3, retaining 1.0.2. The new contract also permits a narrowly pinned
historical numeric-owner restart bridge; it does not claim published session
compatibility for that owner. Unified updates and CLI maintenance reuse existing
contracts and add no versions or builds. Bundle schema 2 is a storage format,
not a wire contract or product version.

## Out of scope

Automatic destructive restart, force-killing uncooperative owners, remote ctld
or taskd restart, and arbitrary remote maintenance RPCs.

## Unresolved questions

None for the implemented maintenance boundary.

## Detailed specifications

- [Component inspection, updates, and restarts](../component-updates.md)
- [Bundle packaging](../ci-bundles.md)
- [Protocol compatibility](../protocol-versioning.md)
