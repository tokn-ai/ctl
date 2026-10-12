# SSH idle lifetime and native authorization reuse

A macOS wake trace exposed two separate events: a private OpenSSH master
expired while idle, and a later protected Keychain read could not reuse its
retained native authorization. Preventing the first event avoids unnecessary
credential reads; it does not prove the second event is resolved.

## Observed sequence

The October 12, 2026 trace used one broker throughout. Times below are local
Asia/Shanghai time. Audit records and native LocalAuthentication logs agree.

| Time | Observation |
| --- | --- |
| 13:03:35 | Full wake |
| 13:05:30 | Interactive saved-credential read succeeded |
| 13:05:32 | SSH master became ready |
| 13:13:12 | OpenSSH reported `ControlPersist timeout expired` |
| 13:19:41 | UI-forbidden read on the retained context failed with OSStatus `-25308`; native evaluation reported that interaction was required |
| 13:19:41 | Broker revoked that approval after the failed read |
| 13:20:29 | A subsequent explicit connection permitted fresh authorization and succeeded |

The original native `LAContext` and its server context were still present at
the failed read. The item had the same access-control constraints. No earlier
sleep, lock, scope revocation, or application context invalidation was observed.
The failed read therefore was not caused by losing the Rust context reference
or revoking application approval before the native operation. The trace does
not establish why macOS required interaction again. In particular, it does not
establish a documented fourteen-minute authorization timeout.

The saved item had legacy `biometryCurrentSet` protection. New saves use
`userPresence`; reads preserve older access control until an explicit resave.
See [credential storage](credentials.md#storage-and-metadata).

## Private-master lifetime fix

Ctld now retains the already-pinned local mux client after startup commits, and
acquires one for an explicitly reused private master. OpenSSH counts that
client as active, suspending `ControlPersist` idle expiry without a remote
session or a credential read. Repeated reuse retains one descriptor per target;
socket pathname and device/inode identity determine whether the lease matches.
Passive observation does not acquire ownership.

Disconnect, endpoint replacement, and ordered shutdown release the lease.
Ordered shutdown releases both private leases and shared anchors explicitly,
even if a finishing task still holds a reference to daemon state. Process death
also closes descriptors. The existing five-minute idle cleanup then resumes
once no clients remain. Network keepalives and sleep/lock credential revocation
retain their existing behavior. Configured shared masters keep their owner's
lifetime policy. No wire contracts or protocol versions change.

An isolated OpenSSH 9.9p1 experiment used a three-second idle timeout and fresh
loopback keys. The unheld master expired in about three seconds. A raw local
mux client kept the same master alive for seven seconds without opening a remote
session. Both closing the client and killing its owner restored expiry in about
three seconds. A separate ignored regression exercises the actual Rust lease
with an isolated `sshd`; ordinary ownership tests cover readiness, cancellation,
replacement, repeated reuse, passive observation, disconnect, and shutdown.

## Controlled native reuse study

The opt-in [native probe](../ctl/keychain-client/README.md) creates a synthetic
protected item and retains one context through a fifteen-minute interval with
no intermediate reads. It compares the current Security query UI flag plus
`LAContext.interactionNotAllowed` with the context property alone, always
forbidding UI after initial authorization. Lock, sleep, or cancellation aborts
the study; normal exit deletes the exact synthetic item.

Run `user_presence` and `biometry_current_set` separately. A result for one
protection policy does not establish behavior for the other. The current
query policy is attempted first: a successful read can refresh native
authorization and affect the second policy. A failed read is recorded rather
than triggering interactive fallback or invalidating the context between
comparisons. Synthetic-item success cannot establish that an existing saved
item with a different history will behave identically.

On this Mac, separate `userPresence` and `biometryCurrentSet` studies both
reproduced refusal after the full fifteen-minute gap. Both query policies
succeeded immediately after initial authorization and then returned `-25308`
from the Security read at 900 seconds.
The console remained unlocked, the monitored session generation remained
unchanged, and the context had not been invalidated by the application. Exact
synthetic-item cleanup succeeded. Removing the deprecated query UI flag alone
therefore does not fix this reproduction. This establishes failure by the
fifteen-minute observation, not an exact native timeout or its underlying cause.
Native logs from the `userPresence` run also confirmed that the original
LocalAuthentication server context survived. Immediate quiet reads skipped
access-control evaluation, while both delayed reads created an imported
context and attempted evaluation, which returned `LAErrorNotInteractive`.
The first context's idle interval included separate authentication activity in
the production broker; it did not include further reads on the probe context.

Apple's published [Security implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/sec/Security/SecItem.m#L1323-L1329)
externalizes a provided context before Keychain IPC, then
[imports a context when authentication is needed](https://github.com/apple-oss-distributions/Security/blob/main/OSX/sec/Security/SecItem.m#L1399-L1416).
This is consistent with the observed path difference. Whether initial explicit
ACL evaluation on the original context changes delayed reuse remains a separate
experimental question; the source alone does not establish a fix.

The initial explicit-ACL experiment could not reach evaluation: the
UI-forbidden attribute-only query for the fresh biometric-only item returned
`-25308`. It displayed no authentication UI and cleaned up the synthetic item.
This is an inconclusive binding experiment, not evidence that public ACL
evaluation fails. A policy-evaluation path can first authenticate the original
context, then fetch and evaluate the exact persisted ACL with UI forbidden;
generic policy success alone must not be treated as permission to read the item.

That original-context policy experiment subsequently authenticated successfully
with `deviceOwnerAuthenticationWithBiometrics`. Using the same context, both the
exact ACL attribute fetch and public `.useItem` evaluation succeeded with UI
forbidden. Immediate secret reads succeeded, but both query policies again
returned `-25308` after the uninterrupted 900-second gap. Session generation,
console-unlocked state, and application invalidation remained unchanged;
synthetic-item cleanup succeeded. Initial public evaluation on the original
context therefore did not resolve this reproduction.

| Synthetic protection | Initial authorization path | Immediate quiet reads | Quiet reads at 900 seconds | Cleanup |
| --- | --- | --- | --- | --- |
| `userPresence` | Implicit Keychain read | Succeeded | Both policies refused with `-25308` | Succeeded |
| `biometryCurrentSet` | Implicit Keychain read | Succeeded | Both policies refused with `-25308` | Succeeded |
| `biometryCurrentSet` | Original-context biometric policy, then exact ACL quiet evaluation | Succeeded | Both policies refused with `-25308` | Succeeded |

The probe also offers `keychain_read_and_acl_binding` to compare implicit
item-specific authentication followed by quiet original-context ACL binding.
That additional mode was not run in this investigation. No native
authentication behavior has been changed in production on the strength of
these experiments. The underlying reason for native refusal remains unresolved.

Native context reuse is documented by Apple's
[authentication-context query contract](https://developer.apple.com/documentation/security/ksecuseauthenticationcontext).
The public [access-control evaluation API](https://developer.apple.com/documentation/localauthentication/lacontext/evaluateaccesscontrol(_:operation:localizedreason:reply:))
can also test a specific item policy, but evaluation may change native state;
it is not a passive inspection of an authorization expiration time.
