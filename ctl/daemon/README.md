# ctld

Per-user SSH connection and credential broker for ctl.

Installs `ctld`, the companion daemon used by `ctl` and the ctmux desktop app.
It owns shared SSH connections, port forwards, and VPN container lifecycles.

On macOS, saved credentials and Touch ID require the signed helper with the
`dev.tokn-ai.ctl.ctld` identity and matching Keychain entitlement/provisioning.
`cargo install ctld` builds the executable but does not configure Apple signing.
The repository's desktop release workflow packages the signed helper when
signing credentials are configured.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
