# ctl-keychain-client

Owned macOS Keychain interfaces for ctl credential storage.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

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

Licensed under MIT.
