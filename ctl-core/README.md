# ctl-core

Shared foundations for ctl and ctmux components:

- `component`: product/source build identity and explicit protocol advertisements.
- `protocol`: canonical `major.minor.build` contracts and explicit-set negotiation.
- `executable`: bounded inspection and verification of replacement executables,
  enabled by the `executable` feature.
- `paths`: the shared `~/.tokn/ctl` storage directory on every platform.
  Resolving it does not create files or import data from former locations.

Workspace builds fingerprint the Rust component sources, dependency definitions,
and embedded shell scripts/skills. Registry builds fingerprint this package's
sources and read revision/dirty information from Cargo's `.cargo_vcs_info.json`.
They do not inspect neighbouring packages or an enclosing Git checkout.

Registry and full-workspace fingerprints are intentionally different. Published
contracts govern compatibility independently of product releases and provenance.
Every later implementation of one major supports all earlier published contracts
in that major. Source identity still pins verified executable replacement. The workspace pins internal dependency
versions to keep registry components on the same release.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
