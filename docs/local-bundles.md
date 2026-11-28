# Local development builds

## Install a native development build

Run `ctlup dev` from the root of the ctl checkout you want to build, including
its uncommitted changes. The command fails if the current directory is outside
that root or in a subdirectory. Use `--workspace /path/to/checkout` to select a
root explicitly. The same rule applies to `--app`, `--ctream`, and remote
development builds.

The native build prepares all six runtime commands and the separate `ctlup`
manager together. On macOS it signs the complete `ctld.app` using the shared
development provisioning profile.

```sh
ctlup dev
ctlup status --json
# Optional Cargo configuration:
ctlup dev --profile release --target-dir target
```

On Windows x64, bootstrap from the MSVC checkout with
`cargo run -p ctlup -- dev`. The builder requires Node 24 and
uses the configured native Rust toolchain. It writes `.exe` files into the
same managed installation store.

The active build lives under `~/.tokn/ctl/ctlup/store/runtime/`; stable commands are exposed
through `~/.tokn/ctl/bin`. Add that directory to `PATH`. Development installation
updates the same active selection as `ctlup update`, so status reports the new
build immediately. Existing daemons and sessions retain their running build;
restart them explicitly when ready to use the replacement.

The macOS and Linux repository launchers bootstrap `ctlup` when needed and
delegate preparation to it. `pnpm desktop:dev` does this before each native
launch, and `pnpm daemons:build` uses the same preparation path on Unix.
On macOS, `pnpm ctld:build` prepares the complete native installation for
CLI work. `ctlup dev`, `ctlup dev --app`, and `ctlup install app --provision`
use the same Apple Team and profile selection. When `ctlup` opens its shared
project on first use, sign in to Xcode, select your Apple Team under
**Signing & Capabilities** for the `ctld-provisioning` target, and build the
target once. Then rerun the original command. Development signing needs no
notarization credentials and disables timestamps.

Use `ctlup sign --provision --team` in a terminal to set up or change the Apple
Team before a build. Press Enter to keep the saved team when prompted. The next
checkout build uses the selected team. Saved credentials are scoped to the
signing team's Keychain identity.

Cargo configuration, custom profiles, `CARGO_TARGET_DIR`, and explicit
`--target-dir` are preserved. The builder consumes Cargo's actual executable
paths and uses a cache under
`<configured-target-dir>/ctl-dev/<checkout-id>/cargo`, keeping bootstrap and
runtime compilation separate. Only targets compatible with the running native
manager are accepted; cross-platform remote bundles use the separate path below.
Build and signing failures leave the active installation unchanged. Cancellation
stops owned build processes; completed activations preserve running services.

## Renew the active development helper's signature

On macOS, run `ctlup sign` from any directory after activating a local runtime
with `ctlup dev`. Renewal uses the active build and requires no checkout or Node.

Renewal reuses the installed `ctld` executable without compiling. It uses an
unexpired development provisioning profile for the helper's Apple Team or
asks Xcode to renew the profile for that team. If `CTLD_PROVISIONING_PROFILE`
points to an expired file, replace it with a current profile or unset the
variable to let Xcode renew it. Restart `ctld` separately to use the renewed
helper.

## Install a desktop development build

```sh
pnpm install --frozen-lockfile
ctlup dev --app
ctlup dev --app --workspace /path/to/checkout --json
```

`dev --app` builds the native debug app with its frontend and bundled daemons
and installs it on this computer. It preserves the selected publication
channel. On macOS it uses the development helper profile for signing and can
renew an expired profile through Xcode. If provisioning or signing fails, the
app can bundle an unsigned helper. Use `ctlup sign --version BUILD_ID` to sign
and notarize a CI desktop build already in the local install cache.
[Release signing](ci-bundles.md#sign-a-ci-built-macos-desktop-release-locally)

## Install a ctream development build

From the checkout root, build and activate the native preview and CLI:

```sh
ctlup dev --ctream
ctlup ctream run
```

If your installed updater predates `--ctream`, substitute
`cargo run --locked -p ctlup --` for `ctlup` in these commands. Build the
remote agent you need with `--agent-target`, for example Windows via WSL from
macOS:

```sh
ctlup dev --ctream --agent-target x86_64-pc-windows-gnu
ctlup ctream run
```

The developer build uses the current checkout, including uncommitted changes,
and activates it in `$CTL_DATA_DIR/ctlup/store/ctream/<build-id>/`. The build ID
uses the latest artifact modification time in UTC, the checkout's short
revision, and the ctream package version. Run a fresh build after upgrading
from the old `ctream/installs/` layout. `ctlup dev` supports
`--workspace`, `--profile`, `--release`, `--target-dir`, and `--json`. A native
Linux or Windows x64 build can include its local agent. On macOS, select a
remote agent target to use the managed host flow. Repeat `--agent-target` to
build several lanes.

Cross builds need the selected Rust targets, Zig, and `cargo-zigbuild`. For
Linux agents use `x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl`.
Native Linux builds include the Wayland helper binaries and require the system
`libxkbcommon` development package. Linux agents built from another platform
use Wayland helpers already installed on the remote host. The preview syncs the
agent when you connect to a host. To return to a workflow build, use
`ctlup ctream update` or install a bundle with `ctlup ctream install --from`.

## Build a local Linux bundle

Run `pnpm bundles:build` from the checkout to build `ctl-agent`, `ctmuxd`,
`ctl-taskd`, `ctld`, `ctl`, `ctmux`, and the separate `ctlup` manager. The command
uses the current working tree, including uncommitted and unpushed changes.
It prepares files for a later install. The default target is
`x86_64-unknown-linux-musl`. For Linux ARM64, run:

```sh
pnpm bundles:build --target aarch64-unknown-linux-musl
```

To build for a Linux host and install in one step, run
`ctlup dev --host NAME` for a saved host or `ctlup dev --ssh ALIAS`
for an OpenSSH alias. Use `ctlup dev --host work --host lab` for multiple saved hosts
and `--workspace` to select a checkout. To install on this computer, run
`ctlup dev` separately. In a desktop app installed with `ctlup dev --app`,
select a Linux x64 or ARM64 SSH host and choose **Build from checkout**.
The app builds from the checkout used to compile it, then uploads to the host.

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

In the component update dialog, choose **Provided build files** and enter the
path to `<target>/`. Leave **Native local build** unchecked for this packaged
Linux build. From the CLI, run `ctlup install --host NAME --from DIRECTORY`
with the same directory.

To use a copied build, keep `<target>/` and `updaters/<target>/` under the same
parent directory. The app and standalone `ctlup` discover the matching updater
in that layout, including outside a checkout. For separately stored updater
files, set `CTLUP_BUNDLES_DIR` to the directory containing `ctlup-<target>.json`
and its binary. The client checks the target, version, revision, and checksum
locally before transferring the required files over SSH. Both computers can
be offline from the publication server. Restart a running daemon separately
when ready to use the installed build.

Failed or cancelled builds do not replace the previous output. Consumers must
check the command's exit status rather than reuse that output after a failure.
Normal cancellation removes the owned build container immediately. If the
launcher is killed abruptly, the container stops after its build lease expires
(normally within 20 seconds, plus process termination grace).
