# Proposal 0011: Credential inventory and scoped reconnect approval

- Status: Implemented
- Created: 2026-10-09
- Updated: 2026-10-13
- Implementation PRs: [#47](https://github.com/tokn-ai/ctl/pull/47),
  [#53](https://github.com/tokn-ai/ctl/pull/53),
  [#77](https://github.com/tokn-ai/ctl/pull/77),
  [#82](https://github.com/tokn-ai/ctl/pull/82),
  [#88](https://github.com/tokn-ai/ctl/pull/88),
  [#108](https://github.com/tokn-ai/ctl/pull/108),
  [#109](https://github.com/tokn-ai/ctl/pull/109),
  [#110](https://github.com/tokn-ai/ctl/pull/110)

## Summary

Desktop Credentials and `ctl passwords` inspect owned credential metadata
without revealing secrets. Signed macOS helpers verify identity passphrases and
perform explicit secret mutations. Successful SSH authentication can retain
scoped Keychain authorization for bounded reconnects without caching secrets.

## Motivation

Users need to inspect and remove saved credentials, understand changed key files,
and reconnect after network loss without repeated authorization prompts. These
features must preserve the difference between metadata, authorization, and
actual credential access.

## Design

SSH passwords and identity passphrases use the shared signed-helper Keychain
namespace. Inventory returns attributes and explicit unknown/unavailable states;
it never turns denied or incomplete discovery into an empty successful list.
The desktop refresh is passive. Importing older metadata is explicit; CLI
attribute discovery may authorize access and refresh its display cache.

A saved identity passphrase is bound to a canonical key path and exact file
contents, verified by a temporary isolated OpenSSH agent. Connection preparation
discovers public identities without reading secrets. Only a signature request
unlocks the required saved key. Changed files invalidate reuse; a neighbouring
public key is a hint, not authorization. Native fallback remains available for
unsupported SSH configuration.

VPN credential identity includes the logical route and remote trust pin, while
excluding the local broker socket. Desktop and CLI brokers can share saved
items without sharing in-memory approval contexts. Legacy lookups remain exact;
opaque historical items are not guessed into another route.

After successful authentication, the broker can retain authorization contexts
for at most 24 hours from their original approval. Reads do not extend the
window. Contexts bind the account, endpoint, route, effective SSH configuration,
host trust, and exact secret selector; key approval also binds file contents.
Secrets are read for authentication and temporary owned buffers are zeroized.
Decrypted keys are retained only within the connection attempt.

Lock, sleep, logout, session changes, broker restart, explicit disconnect, and
owned-secret mutations revoke approval. Updated one-shot helpers publish a
shared nonsecret revision before mutation. Background reconnect uses quiet
master establishment; it never opens authentication or save UI. Missing approval
returns authentication-required. Active `ctl` TUI shells additionally register
their exact routes with the broker while their terminal UI is open. After full
macOS wake and unlock, the broker groups eligible routes into one native
authorization batch. DarkWake and closed or inactive hosts do not start a batch.
The broker prefers exact nonsecret selectors from successful connections. If
those are unavailable after restart or expiry, it may passively discover exact
configured password scopes and saved identity bindings for an already-open
shell route. Unknown, legacy unbound, or unverifiable entries are excluded.
Metadata selects eligible items; fresh native consent authorizes their access.
The current configuration, host trust, identity-file binding, credential revision,
and console session fence the batch. Changed scope evidence requires fresh
consent rather than inheriting an older grant. The batch permits at most one
native interactive read and shares its authorization context for subsequent quiet reads.
It refreshes approval even when an SSH master survived sleep, so a later quiet
reconnect can use it. Failed or canceled access ends the batch without another
automatic prompt until another sleep/wake cycle or an explicit Connect action.
`accept-new` host-key policy disables retained approval for new connections.
Retained reads forbid native UI even for interactive reconnects. A rejected
context or missing retained item ends that attempt with authentication required,
including lazy identity reads that fail without an OpenSSH password prompt.
Fresh authorization requires an explicit connection or the separate full-wake
batch. Audit records typed read modes and revocation reasons; it does not identify
the native biometric method or record password/private-key material.

The TUI applies the connector's retry classification to background refreshes
and pane reconnects, stopping on authentication/configuration failures. A
successful wake authorization or explicit `ctl` connection resumes
authentication-blocked shells for matching registered routes; configuration
failures and manual disconnects stay blocked. Successful explicit session
selection also resets maintenance. A blocked pane shows `disconnected` and
retains the reason retries stopped, rather than claiming it is reconnecting
after automatic recovery has halted. The broker remembers a quiet
authentication-required result until explicit Connect or successful wake
authorization; it can still reuse a live master. Client EOF cancels pending
connection work. Private-master cleanup retains the target lock through bounded
termination, using the original control connection
to protect replacement sockets and reporting termination failures. Authorization
and master ownership commit before the final readiness response. Shared masters
retain their owner's lifetime; cancellation closes only the broker's anchor.
Unknown existing-master observations preserve the endpoint rather than initiating
new authentication. The shell retries passive observations and pane recovery
after `ssh_status_unknown`, including control-command timeouts and observations
superseded by a connection change. Uncertainty cannot grant permission to prepare
a VPN or establish another master. Once observation succeeds, recovery continues
without session reselection or an authorization event. A failed background check
is shown separately from a disconnected pane stream. This client policy retains
the existing ctld contract and error code.
The disconnected live-pane footer puts `Enter to reconnect` before error details.
Plain Enter retries the current pane through its quiet transport, without a
session picker or replacement shell. It clears stale client blockers and retry
deadlines while leaving an already pending attempt running. It does not override
broker authorization or manual-disconnect policy; those errors can reappear.
Copy mode, command prompts, session pickers, prefix commands, and connected or
ended panes retain their existing Enter behavior.

Explicit connection success publishes recovery only after SSH readiness and
ownership commit, including when a new TUI reuses an existing master. Exact-route
watchers can use that master without a new Keychain grant. Equivalent master
policies and remote VPN-owner dependents require current matching scoped
approval before notification. The signal only permits a quiet retry; it cannot
authorize a credential read or open UI. Every publication has a distinct broker
event revision, so successive manual connections in one console session remain
visible. Requests that fail or are canceled before readiness commits do not
publish success.

Each host has an independently revocable grant even when several hosts share
the same native wake context. Disconnecting one host leaves the other grants
valid; session revocation or credential mutation invalidates the shared context.
Shared and privately owned masters for the same effective SSH principal can
share credential approval without sharing their transport lifecycle.
This automatic wake policy covers active `ctl` TUI shells; desktop embedded
terminals retain their existing Connect host flow.

## Invariants

1. Inventory and history never expose passwords, passphrases, or private key bytes.
2. Metadata does not authorize secret reuse or prove that a key is unchanged.
3. Failed authentication grants no reusable approval.
4. Approval is bounded, process-local, scoped, and revocable; secrets are not its cache.
5. Forgetting credentials does not remove key files or terminate existing connections.
6. Quiet reconnects cannot request interactive credential access; full wake may
   start one separate authorization batch for eligible live shell routes.
7. Reusing retained approval cannot silently fall back to fresh authentication UI.

## Protocol impact

After helper 1.1.2/build 2, explicit clear adds `ctld_helper` 1.1.3/build 3;
attribute discovery adds 1.1.4/build 4. Shared mutation revocation adds
1.1.5/build 5, retaining 1.0.1 and 1.1.2–1.1.4. New clients require 1.1.5
before sending secret mutations. Quiet master establishment advances `ctld`
from 1.1.13/build 13 to 1.1.14/build 14, retaining 1.0.12 and 1.1.13.
Older brokers support passive status rather than quiet creation.
Live shell registration advances `ctld` from 1.1.15/build 15 to 1.1.16/build 16,
retaining 1.0.12 and 1.1.13–1.1.15. `watch_recovery` owns a dedicated streaming
socket lease and returns `recovery_registered`, then `recovery_authorized` with
a broker event generation after successful wake authorization or an explicit
committed connection. It carries no credentials or native contexts. Older
negotiated contracts retain quiet reconnect behavior and
receive no watch request. The helper remains 1.1.5/build 5. The lifecycle
contract remains 1.0.1/build 1. Older writers must be replaced before relying
on cross-process approval revocation.

## Out of scope

Secret reveal/copy UI, persistent authorization contexts, private-key storage,
non-macOS Keychain emulation, and guaranteed prompt-free reconnection.

## Unresolved questions

The observed macOS refusal of a previously authenticated context has not been
explained. The broker now makes refusal visible and denies UI during reuse;
24-hour retention still does not guarantee that macOS will honor the context.
The October 12 wake trace confirmed that the same native context was retained
until macOS refused the read. A separate OpenSSH idle expiry caused the
unnecessary reconnect preceding that read. Private-master lifetime leases
address the idle expiry; synthetic delayed-reuse studies cover both current
and legacy item protection without claiming the native refusal is resolved.

## Detailed specifications

- [Credentials, identity verification, and approval](../credentials.md)
- [Connection transitions](../connection-state.md)
- [Protocol versions and mutation compatibility](../protocol-versioning.md)
- [Idle lifetime and native reuse investigation](../connection-authorization-reuse.md)
