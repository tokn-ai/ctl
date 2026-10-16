# ctld

Per-user SSH connection and credential broker for ctl.

Installs `ctld`, the companion daemon used by `ctl` and the ctmux desktop app.
It owns shared SSH connections, port forwards, and VPN container lifecycles.

Committed private SSH masters retain one local control-socket lease while
ctld owns the connection, preventing idle expiry without a remote shell or
credential read. Disconnect and shutdown release it; daemon death restores
OpenSSH's five-minute idle cleanup once no clients remain. Configured shared
masters keep their owner's lifetime policy. See the
[lifetime and authorization investigation](../../docs/connection-authorization-reuse.md).

Active `ctl` TUI shells register their exact SSH routes through the ctld 1.1.16
recovery-watch contract. On macOS, full wake and unlock can request one grouped
native authorization for exact saved-credential bindings of those open shell
routes. It prefers selectors remembered from successful connections and may
passively rediscover configured bindings after restart or expiry. DarkWake and
inactive hosts do not request it. Subsequent reads and SSH recovery are quiet;
cancellation or failure does not start another prompt loop.
Successful explicit `ctl` connections also notify matching open shells after
SSH readiness commits, including reuse of a live master. Notifications allow
quiet retries without granting credential access or opening native UI.
The batch retains independently revocable host grants, without caching passwords,
passphrases, or decrypted keys. Desktop embedded terminals keep their existing
Connect host flow. See [credential approval](../../docs/credentials.md#reconnect-approval).

On macOS, saved credentials and Touch ID require the signed helper with the
`dev.tokn-ai.ctl.ctld` identity and matching Keychain entitlement/provisioning.
`cargo install ctld` builds the executable but does not configure Apple signing.
Official version releases provide a Developer ID signed, notarized, and stapled
`ctld.app` for both macOS architectures. Run `ctl setup` to prepare the selected
helper under `~/.tokn/ctl/components/ctld/`; the desktop app is not required.
Setup preserves the complete bundle. Downloaded official releases update
`current` and the architecture/API selection. Preparing a selected embedded or
checkout app leaves shared selections unchanged. No setup operation restarts the
daemon. When there is no eligible local, shared, or bundled helper, setup
downloads the release matching the CLI's version and architecture.

`CTLD_BIN` explicitly overrides helper discovery. Ordinary macOS debug CLI
builds then prefer the checkout's provisioned signed helper; signed development
CLIs prefer their matching embedded helper. Shared discovery prefers a verified
compatible shared `ctld.app` before a bundled release or nearby desktop app.
With no eligible signed app, macOS selects an official release awaiting explicit
setup. Manually managed or unsigned helpers require `CTLD_BIN`; implicit sibling
and `PATH` discovery is retained on other Unix platforms.
Compatibility requires the native target and explicitly shared ctld, lifecycle,
and helper contracts. Remote VPN routes require ctld `1.1.13` and helper `1.1.2`.
The installed helper's version and source
fingerprint need not match the CLI's. Its signature, provisioning, and build
metadata are still checked against its own installation manifest.

Prepare a checkout helper with `pnpm ctld:build`.
It publishes under `<target-dir>/ctl-dev/helpers/<checkout-id>/` for reuse by an
unsigned Cargo debug CLI, without changing shared selections. Checkout helpers
and embedded signed development builds launch from the immutable
`~/.tokn/ctl/components/ctld/development/<archive-sha256>/` cache. Explicit
development preparation leaves release `current` unchanged. Setup, status,
startup, and restart use one shared Rust resolver, so setup prepares the same
checkout, embedded, shared, or explicit override chosen for activation. Status
only inspects it and reports its source, path, archive hash, and build metadata
separately from the running owner. The desktop prefers its own bundled helper
after any explicit complete-bundle selection. Source builds remain available for
development and other Unix platforms.

A daemon launched by an older client from a deleted build directory can still
serve existing masters, but cannot execute its askpass or isolated-agent helper.
New connections fail with an explicit helper availability error before credential
access. Prepare a valid replacement and use `ctl components restart ctld` when
ready to interrupt its connections and VPNs; updating files does not restart it.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
