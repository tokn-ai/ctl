# ctld

Per-user SSH connection and credential broker for ctl.

Installs `ctld`, the companion daemon used by `ctl` and the ctmux desktop app.
It owns shared SSH connections, port forwards, and VPN container lifecycles.

On macOS, saved credentials and Touch ID require the signed helper with the
`dev.tokn-ai.ctl.ctld` identity and matching Keychain entitlement/provisioning.
`cargo install ctld` builds the executable but does not configure Apple signing.
Official version releases provide a Developer ID signed, notarized, and stapled
`ctld.app` for both macOS architectures. Run `ctl setup` to install the helper
matching the CLI's release under `~/.tokn/ctl/components/ctld/`; the desktop app
is not required. Setup preserves the complete bundle and switches `current`
atomically without restarting the daemon. Source builds remain available for
development and other Unix platforms.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
