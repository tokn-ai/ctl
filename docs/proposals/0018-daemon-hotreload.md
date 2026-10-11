# Proposal 0018: Replace daemon code while preserving live work

- Status: Proposed
- Created: 2026-10-12
- Experimental proof: [Unix PTY executable replacement](../../ctmux/daemon/examples/README.md)

## Summary

Add explicit `ctld hotreload` and `ctmuxd hotreload` operations that activate a
verified replacement without restarting SSH connections, VPN tunnels, shell
processes, or PTYs. Brief automatic client reattachment is acceptable. The old
daemon code must disappear; retaining old resource workers is not an acceptable
implementation of this feature.

This extends [Proposal 0012](0012-component-maintenance.md). Installation,
destructive restart, and live replacement remain separate operations.

## Motivation

Selecting a newer binary currently leaves a compatible running owner in place.
The existing restart paths deliberately terminate resources: ctld stops VPN
ownership and forwards, while ctmuxd terminates every session. Neither operation
can activate new code while preserving ongoing work.

## Design

Investigate full Unix program-image replacement using `exec`, with an explicit,
versioned runtime export/import format and an allowlist of inherited descriptors.
This replaces the executable and threads in place: the daemon PID remains the
same, preserving parenthood of existing shell and SSH children. No old frontend,
PTY owner, reaper, authorization worker, or forwarding worker remains running.
The user accepts keeping the same PID when all old code is replaced.

This is a feasibility direction, not implemented hotreload support. An exec call
alone is insufficient: Rust/Tokio objects, parser state, relay buffers, leases,
and timers disappear when the program image changes.

Keep three private implementation boundaries inside the existing daemons:
component-owned semantic state export/import, an ownership-aware Unix resource
adapter, and one reload coordinator with an early restoration bootstrap.
Ordinary domain commands use the existing admission boundary rather than growing
individual reload branches. Client protocol DTOs and renderer dumps are not the
runtime state format. This structure must precede production activation support.

The isolated first proof is present as two executable examples and integration
tests. It validates actual image replacement, an unchanged PTY/shell, partial
output restoration, continued input, repeated exec, a returned exec error, and
original-child reaping. This does not accept or implement production hotreload;
the proposal remains Proposed while full runtime migration is unproven.
The initial OS proof passed on macOS and Linux, and strict workspace Clippy
passed. No production daemon or connection was changed by the experiment.

The next component gate is implemented: AVT exports/imports complete opaque
emulator and parser state, and ctmux-core exports/imports input-mode and
alternate-screen observer state. Thirteen AVT state tests cover a serialized
prefix checkpoint followed by a suffix compared with uninterrupted processing,
including partial controls, inactive screen buffers, resize, and deferred history
cleanup. Import validates invariants and tunable resource bounds; encoded input
must also be bounded by its caller before deserialization.

The A/B proof now carries these state exports and separate pending UTF-8 bytes
through executable replacement, checking continuation against uninterrupted raw
output replay. This proves the emulator/observer boundary only. Full daemon
physical/logical history and journal, session registry, attachment tokens and
leases/deadlines, shell-reporter FIFO state, managed tasks, and multithreaded
quiescence remain future work. Ctld's forwarding, SSH, VPN and helper resource
handoff is also unimplemented. The proposal remains Proposed.

The composed state-transfer proof passes on macOS and Linux, including six actual
image replacements. Existing daemon observer tests, AVT/core tests, strict
workspace Clippy and formatting checks pass. Protocol changes: none.

The first platform targets are macOS and Linux. Windows hotreload remains
unsupported until complete ConPTY ownership transfer is demonstrated through a
supported mechanism. The new operation refuses unsupported owners, resources,
and state formats without silently invoking destructive restart.

Ctld adopts the same OpenSSH masters and VPN containers, preserves anchor pipes,
and restores active forwarding streams including buffered bytes and half-close
state. Established `ctld --proxy-route` helpers also execute old ctld code;
complete replacement must upgrade those helpers or refuse while unsupported
helpers remain active. Existing OpenSSH and VPN engine programs are external
resources, not components upgraded by daemon hotreload.

Ctmuxd retains the original PTYs and shell children, imports all terminal and
history state, and restores session/view/terminal identities, layouts, revisions,
logical attachments, leases, deadlines, shell reporters, and task associations.
Renderer checkpoints are not complete runtime backups.

On macOS, existing network connections survive without reading credentials.
The native `LAContext` cannot be preserved by copying application approval
metadata. The user permits fresh Touch ID on the next protected credential use
after hotreload. Hotreload itself must not open authentication UI, retain old
authorization code, export plaintext credentials, or manufacture replacement
consent. In-flight native authentication makes preparation busy.

The daemon commands and proposed `ctl components hotreload COMPONENT` adapters
use the shared Rust client API. Remote control remains machine/account pinned
through fixed maintenance operations. Passive status reports executable build
and reload generation independently of PID and installed bytes.

## Invariants

1. Hotreload preserves existing live resources; ordinary restart stays explicit
   and destructive. There is no hidden cold-restart fallback.
2. Successful activation leaves no old selected-daemon executable image or old
   resource worker. Same-PID exec is reported accurately.
3. Exactly one owner reads, writes, mutates, or reaps each resource. Descriptor
   inheritance does not bypass owner validation or lifetime fencing.
4. A consumed output/relay byte is exported or committed before quiescence;
   input and uncertain mutations are never replayed.
5. Session identities, output sequences, history generations, lease ownership,
   child identities, VPN container IDs, and SSH correlation IDs stay stable.
6. Disconnect, pause, authentication-required, sleep, and credential/trust
   revocation policies are not cleared merely by changing executable generation.
7. Candidate validation, incompatible state, and unsupported resource failures
   preserve the original owner. Reload uncertainty is inspected by reload ID,
   not automatically repeated.
8. If exec returns an error, the old image can resume. After the kernel passes
   its replacement boundary, no old in-memory owner remains; failures during
   image replacement or arbitrary replacement crashes are not promised to be
   recoverable without losing daemon-owned resources.

## Protocol impact

Protocol changes: none. No named client/daemon contract, internal protocol build,
or product release changes. AVT emulator/parser and ctmux-core observer state
representations each start at version 1; the isolated A/B proof uses private
schema 2. These checkpoint formats are separate from wire protocols and do not
introduce a production persistence format or hotreload capability.

Future control operations would extend `ctld_lifecycle` (currently 1.0.1/build 1)
and `ctmux_control` (1.0.1/build 1). Remote hotreload would extend
`ctl_maintenance` (1.1.3/build 3). Exact new versions/builds must be assigned when
those operations and capabilities are accepted; existing contracts remain
available and do not imply hotreload support.

The intended data paths retain `ctld` 1.1.16/build 16 and `ctmux` 1.1.20/build 20
unless implementation requires a separate published addition. The private runtime
checkpoint has an independent compatibility schema; it is not a product version
or evidence that two executables can transfer native objects.

## Out of scope

Retaining old resource workers, upgrading OpenSSH or a VPN engine image,
restoring sessions after an arbitrary daemon crash or machine restart,
force-killing unsupported owners, and retrofitting handoff into already-running
legacy binaries that cannot cooperate.

## Unresolved questions

- What bounded quiescence and restoration budget can be demonstrated on both
  macOS and Linux with representative terminal and forwarding loads?
- Which runtime checkpoint formats will supported replacement pairs accept,
  including deliberate exec-back after a safe import failure?
- What supported Windows mechanism, if any, can satisfy full ConPTY takeover?

## Detailed specifications

- [Hotreload investigation and proof gates](../daemon-hotreload.md)
- [Component maintenance](../component-updates.md)
- [Protocol versioning](../protocol-versioning.md)
- [Terminal protocol](../ctmux-protocol.md)
- [Credentials and scoped approval](0011-credentials-reconnect.md)
