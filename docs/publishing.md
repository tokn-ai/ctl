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
ctl-core
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
# macOS client: ctl, terminal/task daemons, then the signed connection helper.
cargo install --locked ctl-cli ctmuxd ctl-taskd
ctl setup

# Other Unix clients: source-built connection, terminal, and task daemons.
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

On macOS, `cargo install ctld` builds an unsigned source executable. It does not
acquire the Developer ID signature, provisioning profile, or Keychain entitlement
needed for Touch ID-protected saved credentials. `ctl setup` installs the complete
signed and notarized helper from the GitHub release matching the CLI's Cargo
version and architecture. That release must be published before setup can
succeed; a draft or an Actions artifact is not an installation source.

The dedicated macOS [CLI release build](ci-bundles.md#build-a-cli-with-ctld-embedded)
embeds the full signed helper in the `ctl` executable. That distribution prepares
its helper from embedded bytes without network access and uses them for
`ctl setup` too. This is separate from crates.io: ordinary `cargo install ctl-cli`
still builds a CLI that obtains its signed helper through setup.

The helper is installed without `sudo` at
`~/.tokn/ctl/components/ctld/versions/<version>-<target>/ctld.app`; the component's
`current` symlink is updated atomically after verification. Remote agent
`~/.tokn/ctl/versions/` and `current` paths are independent. Setup never restarts
the daemon or existing connections. Repeating setup checks and reuses the same
immutable version when its release metadata matches.

`CTLD_BIN` continues to override discovery. Otherwise, macOS prefers the signed
desktop bundle's helper, then an embedded CLI helper, then the managed
installation, then a sibling or `PATH`
executable. Setup obtains its expected Apple Team ID from the fixed
`https://github.com/tokn-ai/ctl` release manifest over HTTPS; it does not contain
a separately compiled vendor Team ID pin. It checks the archive hash and size,
Apple's Developer ID certificate chain and application/team identity, embedded
distribution provisioning, and Gatekeeper notarization before executing the
helper's metadata query. It verifies that build/protocol identity before
selecting the helper.

Publish the [signed macOS release assets](ci-bundles.md) alongside the crate
family. Version-tag CI requires both Mac architectures, distribution signing,
notarization, and a stapled ticket. npm and PyPI installers can consume those
same signed release archives; language-specific installers are not yet provided.

Registry builds use Cargo's archive provenance and a fingerprint of the shared
`ctl-core` package. Full checkout builds fingerprint the component
workspace, including embedded resources. Their fingerprints differ even for
the same commit; install components from the same distribution together.
