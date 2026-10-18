# ctl-keychain-client

Owned macOS Keychain interfaces for ctl credential storage.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Native Keychain operations, dependencies, and session notification adapters are
compiled only on macOS. The platform-independent session approval state machine
is tested on every supported platform; Windows and Linux do not expose Keychain
operations or link Apple frameworks.

On macOS, `with_session_monitor` receives console lock and system sleep events
on the main run loop. `SessionState::subscribe` revokes credential approval;
`subscribe_resume` emits a `SessionResume` once per monitored sleep after the
screens wake and the active console is unlocked. Background maintenance wakes
and ordinary unlocks do not emit resume events. Resume callbacks only enqueue
work; consumers call `resume_is_current` before acting to check the generation,
display state, and current console state. Notifications carry no credentials or
authorization.

The ignored native authorization test requires an entitled, signed macOS test
binary and a user present to authenticate. It creates two protected synthetic
items, permits UI for the first read, then verifies same-item reuse and access
to the second item with UI forbidden. It also checks rejection after context
invalidation and with a fresh unauthenticated context, and deletes both test
items on exit. Ordinary test runs skip this interactive check.

The standalone `native_reuse_probe` target studies delayed reuse on the real
main run loop. Ordinary runs skip it before accessing Keychain. Build it with
`cargo test -p ctl-keychain-client --test native_reuse_probe --no-run`, then
package the reported executable as an isolated, entitled app using
`scripts/ci/package-ctld-app.sh` and a valid local ctld provisioning profile.
Keep that app separate from the active daemon and helper selection.

Run its `Contents/MacOS/ctld` executable with `CTL_RUN_NATIVE_REUSE_PROBE=1`.
It creates a uniquely named synthetic item, permits one initial authorization,
then reads with UI forbidden using the same retained context. The default
schedule is immediate reuse followed by a 15-minute gap with no intervening
reads. `CTL_KEYCHAIN_REUSE_PROBE_OFFSETS=0,900` selects explicit elapsed seconds;
extra timepoints can refresh native authorization and change the experiment.
`CTL_KEYCHAIN_REUSE_PROBE_PROTECTION=user_presence` is the default new-save
policy; `biometry_current_set` tests older biometric-only saves. Run each policy
separately and record it with the result; their authorization behavior need
not be identical.

`CTL_KEYCHAIN_REUSE_PROBE_AUTHORIZATION=keychain_read` is the default initial
authorization path. `access_control_evaluation` instead fetches the exact
persisted ACL with UI forbidden and evaluates it on the original context once.
This mode can stop before displaying UI when the ACL attributes themselves
require authorization; it never substitutes a newly created policy. Native
LocalAuthentication errors have separate domain/code fields from Security
`os_status` values.
`policy_evaluation` permits one native policy authentication on the original
context (`deviceOwnerAuthenticationWithBiometrics` for biometric-only items,
`deviceOwnerAuthentication` for user presence). It then fetches and evaluates
the exact persisted ACL with that same context and UI forbidden. A successful
generic policy evaluation alone never starts the secret-read study. The
`succeeded` field covers native results that do not have a Security OSStatus.
`keychain_read_and_acl_binding` preserves the initial item-specific Keychain
authentication and then quietly evaluates that exact ACL on the original
context. It supports a separate comparison of that proposed binding path;
ordinary runs do not execute any of these interactive modes.

The JSON observations compare the current query UI flag plus
`LAContext.interactionNotAllowed` against the context property alone. The
current policy runs first because a successful read can refresh authorization
and affect the second policy. OS refusal is an observation, not a successful
test or an automatic interactive retry. Locking or sleeping aborts the probe.
Set `CTL_KEYCHAIN_REUSE_PROBE_CANCEL_FILE` to a fresh, initially absent marker
path; creating that file cancels the initial prompt or the wait. Normal exit
deletes exactly the synthetic item with UI forbidden, and reports the cleanup
status and selectors. Inspect that status after any cancellation. Killing the
process cannot run its cleanup.

Licensed under MIT.
