# Local development builds

## Install a native development build

`ctlup dev` builds and installs the current checkout, including uncommitted
changes. It prepares all six runtime commands and the separate `ctlup` manager
as one verified source cohort. On macOS it signs the complete `ctld.app` using
the shared development provisioning profile.

```sh
ctlup dev
ctlup status --json
# Optional Cargo configuration:
ctlup dev --profile release --target-dir target
```

The active build lives under `~/.tokn/ctl/installs/`; stable commands are exposed
through `~/.tokn/ctl/bin`. Add that directory to `PATH`. Development installation
updates the same active selection as `ctlup update`, so status reports the new
build immediately. Existing daemons and sessions retain their running build;
restart them explicitly when ready to use the replacement.

The macOS and Linux repository launchers bootstrap `ctlup` when needed and
delegate preparation to it. `pnpm desktop:dev` does this before each native
launch, and `pnpm daemons:build` uses the same preparation path on Unix.
On macOS, `pnpm ctld:build` prepares the same complete native installation for
CLI work; it is unnecessary before `desktop:dev`. Provision once with
`pnpm ctld:provision`, choose a team in Xcode, and build the provisioning target.
Development signing needs no notarization credentials and disables timestamps.

`ctlup app install --local` and `ctlup app update --local` use the same helper
profile discovery and signing preparation. After the Xcode provisioning project
has a configured team, both build paths can renew an expired profile through
Xcode. On macOS, native runtime installation requires a successfully signed helper.
The app build can bundle an unsigned helper if provisioning or signing fails.

Cargo configuration, custom profiles, `CARGO_TARGET_DIR`, and explicit
`--target-dir` are preserved. The builder consumes Cargo's actual executable
paths and uses a cache under
`<configured-target-dir>/ctl-dev/<checkout-id>/cargo`, keeping bootstrap and
runtime compilation separate. Only targets compatible with the running native
manager are accepted; cross-platform remote bundles use the separate path below.
Build and signing failures leave the active installation unchanged. Cancellation
stops owned build processes; completed activations preserve running services.

## Build a local Linux bundle

Run `pnpm bundles:build` from the checkout to build `ctl-agent`, `ctmuxd`,
`ctl-taskd`, `ctld`, `ctl`, `ctmux`, and the separate `ctlup` manager. The command
uses the current working tree, including uncommitted and unpushed changes.
It prepares files for a later install. The default target is
`x86_64-unknown-linux-musl`. For Linux ARM64, run:

```sh
pnpm bundles:build --target aarch64-unknown-linux-musl
```

To build for a saved Linux host and install in one step, run
`ctlup --host NAME update --working-tree` from the checkout. The desktop
development build offers **Build from checkout** in the update dialog.

Install Zig, `cargo-zigbuild`, rustup, Make, and Perl with `FindBin` and
`IPC::Cmd` for a host build. The script installs the Linux Rust target on first
use. Packaging reads the source and protocol metadata linked into each static
ELF, checks its architecture, and verifies that the runtime executables and
updater all come from the same working tree. This path runs entirely on the
host, including on macOS.

The musl build compiles OpenSSL from source and links it statically into `ctl`
and `ctlup`.
At runtime, HTTPS uses the remote system's installed CA certificates.

When the host tools are unavailable, Docker or Podman compiles inside a Linux
container with Rust 1.99, Node 24, and musl for the chosen architecture. Set
`CTL_CONTAINER_ENGINE` to choose an engine. Building for a different
architecture in a container requires the engine's emulation support. Host
builds reuse `target/development-bundle-build`. Container builds use a compiled
artifact volume for this checkout and target, and shared Cargo download volumes.

The output is a complete local bundle at:

```text
target/development-bundles/<target>/
  bundle.json
  ctl-agent
  ctmuxd
  ctl-taskd
  ctld
  ctl
  ctmux
target/development-bundles/updaters/<target>/
  ctlup-<bundle-id>-<target>
  ctlup-<bundle-id>-<target>.sha256
  ctlup-<target>.json
```

Use that directory as **Provided build files** in the component update dialog,
or pass it to `ctlup update --host NAME --from DIRECTORY`. Do not
enable **Native local build**: the directory is already packaged for Linux.
Updates preserve running sessions; restarting an existing daemon remains a
separate action.

Debug checkout builds discover the adjacent target updater automatically. When
using these files with a standalone `ctlup` or an offline desktop build, retain
the updater sidecars and set `CTLUP_BUNDLES_DIR` to their directory. The updater
metadata and bytes are verified before SSH bootstrap; no remote download is
required. Copying only the six runtime executables is insufficient to bootstrap
an updater for a foreign target offline.

Failed or cancelled builds do not replace the previous output. Consumers must
check the command's exit status rather than reuse that output after a failure.
Normal cancellation removes the owned build container immediately. If the
launcher is killed abruptly, the container stops after its build lease expires
(normally within 20 seconds, plus process termination grace).
