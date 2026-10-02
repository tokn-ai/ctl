# Cargo publishing

The Rust package family starts at `0.1.0`, uses MIT, and supports Rust 1.97 or
newer. Public packages explicitly target crates.io. `ctmux-app` is a desktop
bundle and stays unpublished; its release workflow is separate.

Internal dependencies specify both a checkout path and an exact registry version.
Update the workspace version and every internal dependency requirement together
for each release. Clients and daemons share protocols and build identity, so
release and install the component family together.

## Verify before publishing

From a clean checkout, with Node.js 24 and Rust 1.97 or newer:

```sh
node --experimental-strip-types scripts/ci/verify-cargo-packages.mts
```

The verifier checks package metadata, exact internal dependency versions, and
per-package copies of the root MIT license. It packages every public member,
unpacks the generated `.crate` archives outside the checkout, and runs their
tests with all features. Only archived sources are used; the unpublished family
is linked through temporary crates.io patches. The original external dependency
versions are retained from `Cargo.lock`.

Use `--allow-dirty` for a development check or `--offline` when dependencies are
already cached. CI runs the verifier on Rust 1.97. This command does not upload
anything or need a registry token. Its archive test is separate from Cargo's
workspace temporary-registry verification.

Before the actual release, ensure the publishing account has rights to each
package name, review package contents, and authenticate with `cargo login` or
the registry's token environment variable. A dormant existing crate still
requires publishing rights from its owners.

## First release order

For the first publication, publish packages in the following order, waiting
until each package is available in the registry before its dependants:

```text
ctl-component-info
ctl-keychain-client
ctmux-process-info
ctl-proto
ctl-task-proto
ctmux-proto
ctl-ipc
ctl-task-ipc
ctl-task-store
ctmux-client
ctmux-core
ctmux-ipc
ctl-task-client
ctld
ctmuxd
ctl-client
ctl-taskd
ctmux-tui
ctl-agent
ctmux-cli
ctl-task-cli
ctl-cli
```

This order covers target-specific and development dependencies as well as normal
dependencies. For each package, review a dry run before uploading:

```sh
cargo publish --locked --dry-run -p PACKAGE
cargo publish --locked -p PACKAGE
```

An individual package's dry run needs its internal dependencies to have already
been published. The archive verifier above handles the entire unpublished
family locally. Do not bypass verification with `--no-verify` when publishing.

## Install published commands

After publication, install the required companion executables explicitly:

```sh
# Client machine: ctl plus its connection, terminal, and task daemons.
cargo install --locked ctl-cli ctld ctmuxd ctl-taskd

# Remote machine: SSH gateway plus persistent terminal and task daemons.
cargo install --locked ctl-agent ctmuxd ctl-taskd

# Standalone terminal multiplexer.
cargo install --locked ctmux-cli ctmuxd
```

Cargo installs the selected package's binaries, not dependency binaries. Keep
companions in the same binary directory, or use the documented executable path
overrides. The `ctl-task-cli` package is a command library embedded in `ctl`;
install `ctl-cli` for the `ctl task` command.

On macOS, Cargo-installed `ctld` does not acquire the signing identity,
provisioning profile, or Keychain entitlement needed for Touch ID-protected
saved credentials. Use the signed desktop distribution for those capabilities.

Registry builds use Cargo's archive provenance and a fingerprint of the shared
`ctl-component-info` package. Full checkout builds fingerprint the component
workspace, including embedded resources. Their fingerprints differ even for
the same commit; install components from the same distribution together.
