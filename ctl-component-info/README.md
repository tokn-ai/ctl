# ctl-component-info

Build identity and executable metadata shared by ctl and ctmux components.

Workspace builds fingerprint the Rust component sources, dependency definitions,
and embedded shell scripts/skills. Registry builds fingerprint this package's
sources and read revision/dirty information from Cargo's `.cargo_vcs_info.json`.
They do not inspect neighbouring packages or an enclosing Git checkout.

Registry and full-workspace fingerprints are intentionally different. Install
components from the same release and distribution together; protocol versions
still govern transport compatibility. The workspace pins internal dependency
versions to keep registry components on the same release.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
