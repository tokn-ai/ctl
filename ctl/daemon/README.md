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
is not required. Setup preserves the complete bundle, updates the release
`current` symlink, and selects it for its architecture and APIs without
restarting the daemon.

Standalone macOS CLI builds prefer a verified compatible shared `ctld.app` after
an explicit `CTLD_BIN` override, before their own bundled or loose helper.
Compatibility requires the native target and explicitly shared ctld, lifecycle,
and helper contracts. Remote VPN routes require ctld `1.1.13` and helper `1.1.2`.
The installed helper's version and source
fingerprint need not match the CLI's. Its signature, provisioning, and build
metadata are still checked against its own installation manifest.

Signed development builds use the immutable
`~/.tokn/ctl/components/ctld/development/<archive-sha256>/` cache and update
`selected/<target>-ctld1-lifecycle1-helper1` while leaving release `current`
unchanged. Ordinary Cargo CLI builds can reuse that selection. The desktop
continues to prefer its own bundled helper. Source builds remain available for
development and other Unix platforms.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
