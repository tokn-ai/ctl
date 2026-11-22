# Components and updates

Open **About ctmux** to inspect components on this computer and saved remote
hosts. **Running** identifies the existing process; **On disk** identifies the
selected executable. Build IDs distinguish development binaries that share a
product version. Different builds do not establish which one is newer.

Local components stay expanded. Remote hosts start as a compact table with
summaries of outdated builds, protocol mismatches, and required restarts.
Expand one remote host at a time to see its components. Matching reported
running and installed builds share one row; different builds show separate
**Running** and **On disk** rows. Unknown or stopped running builds are hidden,
leaving the on-disk build and component status. Active-connection metadata
remains labeled **Last observed**. Shared SSH inspection failures appear once
on the host with a short message; hover it for the full diagnostic.

Each protocol has its own line and status: a green tick for the app's current
contract, a yellow tick for a different contract that shares an explicitly
supported version, and a red cross for no shared contract. Unreported protocols
and historical numeric protocols have a neutral question mark. The tooltip
shows the supported and required contracts; matching major versions alone do
not imply compatibility.

Refresh reuses existing authenticated SSH connections and checks service owners
without attaching a terminal, starting a daemon or VPN, or installing files.
An offline saved host stays visible as **Not checked**, with unknown running
versions. Saved identity metadata is never presented as a live process version.
Connection status is separate from inspection support. A **Connected** host can
show **Update agent** when its installed agent lacks maintenance contract
`1.1.3`, which adds companion inspection. Select and upload a complete bundle
that supports this contract; reconnecting the same older agent cannot add it.
The installed agent's own build remains visible when it can be read. A connected
host without a verified saved identity asks for **Check host** to verify its account.
A legacy numeric protocol is labeled **legacy**, independently of published
contract versions.

## Installed channels and active runtime

`ctlup` installs and updates complete builds. `ctl` and `ctmux` perform runtime
operations. The desktop uses the same Rust installation engine as `ctlup`.

Each channel selects one publication containing complete bundles for its
installed targets. A target component contains `ctl`, `ctmux`, `ctl-agent`,
`ctmuxd`, `ctl-taskd`, and `ctld`. Each account selects one native bundle for
active commands. Builds are immutable and verified against
their file checksums, build identity, target and shared protocol contracts.
Foreign-target binaries are never executed during download or import.

The example uses the default data root. With `CTL_DATA_DIR` set, the updater
root is `$CTL_DATA_DIR/ctlup`.

```text
~/.tokn/ctl/
  bin/                              # public ctl, ctmux, and ctlup launchers
  apps/                             # public desktop launch entries
  ctlup/
    state/installations.json        # active selections, sources, and retained builds
    state/runtime-build-paths.json  # maps exact runtime IDs to installed paths
    state/updater-object-paths.json # maps exact updater IDs to installed paths
    state/desktop-object-paths.json # maps exact desktop IDs to installed paths
    state/leases/                   # builds used by running processes
    store/runtime/<build>/<target>/ # verified complete runtime
    store/desktop/<build>/          # verified native desktop package
    store/updater/<build>/          # independent ctlup executable for this machine
    cache/downloads/                # verified publication archives
    cache/unpacked/                 # verified extracted runtime bundles
    cache/updaters/                 # verified downloaded updater bytes and metadata
    staging/
```

The registry records one active native runtime, desktop, and updater per
profile. Launchers resolve selected payloads through the registry.
Development registrations pin the latest build for each canonical checkout and
target.

Runtime, updater, and desktop directories show the installation's source,
version, and short revision. The full build label groups the targets of each
runtime build. Desktop and updater installations are native to this machine,
so their payloads live directly in the build directory. Updaters downloaded
for other targets use the cache.
Stable build names omit the timestamp. Nightly and development names use UTC
build time, with a source suffix for local and PR development builds.

```text
stable-v0.1.4-ba16876
nightly-20261121T081525Z-v0.1.4-ba16876
dev-20261121T081525Z-v0.1.4-ba16876-local
dev-20261121T081525Z-v0.1.4-ba16876-pr-192
```

Exact release pins and files supplied by the caller use `release-` and
`provided-` prefixes. Standalone updaters use `provided-` and their install
time when publication provenance is unavailable. A short digest distinguishes
different builds with the same name. The full digest identifies and verifies
the build for transfers and cache reuse. The directory name remains stable
while the build is registered.
`ctlup status` orders stable builds by SemVer and nightly and development builds
by build time within their groups.

Installations assemble and verify payloads in `staging/`, then publish the
complete directories before committing a selection. Running
processes keep their original builds through leases. Public commands are
available through `~/.tokn/ctl/bin` or the selected profile's `bin`.

The new store starts on first install. Earlier digest-named and target-specific
updater objects and target-first runtime objects keep their original paths
while they are retained. Desktop packages that support layout 7 use build
labels. Older desktop packages and raw archives supplied with `ctlup install
app --from` use digest directories, which lets older running applications hold
their leases. Previously installed desktops retain their original paths.

## Install and update

```sh
ctlup install --channel stable
ctlup install --ssh 'ssh://operator@windows.example:2222' --remote-platform windows
ctlup install --host work --release PUBLICATION_ID
ctlup install --host work --pr PR_NUMBER
ctlup install --from /path/to/bundle-set
ctlup update --host work --host lab
ctlup update --check
ctlup status --host work --json
ctlup prune --forget-missing
```

`install` chooses the source and can provision an account that has no ctl
components yet. Supply an SSH address or OpenSSH alias with `--ssh`, or select a
saved ctl account with `--host`. No selector means this computer. The explicit
sources are a published channel, an immutable release ID, a PR build, or files
from this computer. Each channel or pin is recorded on the destination for the
selected package.

`update` follows the recorded channel. Explicitly pinned builds stay on the
selected build. If a recorded bundle directory disappears, `status` shows the
missing build and `update` can repair it from the exact verified cache when
available. Install from an explicit source to select a replacement. Ordinary
pruning keeps records and available recovery copies for missing builds.
`prune --forget-missing` clears those records.

On Windows SSH, use `--remote-platform windows` for a temporary destination.
Saved hosts use the platform on the connection method. Use the default
`cmd.exe` shell on the server. The runtime is supported on Windows x64.

An offline runtime archive needs its matching `bundle-set.json` or
`bundle-set-windows.json`. Foreign-target builds need their matching updater
sidecar. For a macOS native build, `--local-build --ctld-package PATH` supplies
the six runtime executables and the verified signed helper package. A flat SSH
archive cannot supply the locally signed helper.

## Publications and retained bundles

```sh
ctlup release list --channel nightly
ctlup release show PUBLICATION_ID
ctlup release show --pr PR_NUMBER
ctlup bundle list
ctlup bundle fetch --target aarch64-unknown-linux-musl --channel nightly
ctlup bundle remove --target aarch64-unknown-linux-musl --channel nightly
```

Bundle fetching retains a complete target and its matching updater for
deployment. It preserves the active native commands and can stage foreign
targets without executing them. The runtime status also lists retained and
missing builds with their source metadata. Publication IDs distinguish builds
that share a version string.

## Install or update the desktop app

```sh
ctlup install app --channel stable
ctlup install app --release PUBLICATION_ID
ctlup install app --pr PR_NUMBER
ctlup install app --from /path/to/desktop-package
ctlup update app
ctlup update app --check
ctlup status app
```

The app is installed on this computer and follows its own recorded publication
channel. Local or PR packages stay pinned. Stable packages come from the tagged
release. Nightly and PR packages use the verified GitHub Actions artifact for
the selected revision. macOS signatures and Windows portable package checks
remain part of package verification.

Open the desktop from the profile's `apps/` directory. The public launch entry
is `ctmux.app` on macOS, `ctmux.AppImage` on Linux, or `ctmux.lnk` on Windows.
Use `ctlup dev --app` to build and install the desktop from a checkout.

## Native development builds

```sh
ctlup dev --workspace /path/to/checkout
ctlup dev --release
ctlup dev --profile custom --target-dir /path/to/build-output --json
ctlup dev --app
ctlup dev sign
```

`dev` requires a ctl checkout root as the current directory or as the path
passed to `--workspace`. It builds the six native runtime components and the
independent updater from that checkout. On macOS it signs and verifies the
complete `ctld.app` package.

Each canonical checkout path has a SHA-256 identity and one current build.
Rebuilding the checkout replaces that registration, and other checkouts keep
their latest builds. Local runtime installs retain a compatible updater when
the bundle supplies an older one. For SSH updates to an account with checkout
registrations, choose a publication or supplied build with a compatible updater.
The SSH installer checks for layout support before replacing the target updater
or activating the build.

Only a successful complete build becomes active; compilation, signing or
verification failure leaves the previous installation and running services
intact. `ctlup status` then reports that development installation.

`ctlup dev --app` builds and installs the native desktop app, including its
frontend and bundled daemons. It preserves the saved publication channel.
Install the checkout's frontend dependencies with `pnpm install --frozen-lockfile`
using the Node and pnpm versions in [Tests and CI](testing.md). Windows app
builds also require the matching Rust MSVC toolchain, Visual Studio C++ build
tools, and the Windows SDK.

On macOS, both development paths look for an unexpired helper provisioning
profile and can renew it through Xcode when the project has a configured team.
Runtime activation requires a signed helper. The desktop build can bundle an
unsigned helper if provisioning or signing fails.

On macOS, run `ctlup dev sign` from a ctl checkout to re-sign the
active local development helper without recompiling any runtime component.
The updater reuses an unexpired profile for the helper's Apple Team or asks
Xcode to renew it using that team. Xcode must be signed in for automatic
renewal. You can also supply a matching profile with `CTLD_PROVISIONING_PROFILE`.
The renewed helper gets a new bundle identity when the signed files change.
It keeps the original build date and channel and checkout registrations. Restart
`ctld` separately to use the renewed signature.

The desktop development launcher bootstraps a private native `ctlup` executable
with Cargo, then delegates preparation to `ctlup dev`. It passes Cargo's original
arguments and working directory, preserving custom profiles, output directories
and configuration files. The launched GUI receives immutable paths from the
resulting installation and holds a lease for its lifetime. Each signed macOS
worktree retains its isolated daemon endpoint, and existing GUI or daemon
processes keep their original build until an explicit restart or successful GUI
replacement. `pnpm ctld:build` and `pnpm daemons:build` use this same preparation
path on Unix. Windows retains its existing daemon build fallback.

## Downloads and cache

Updates resolve the selected channel once, then verify the target runtime archive
and its updater from the same immutable publication. Archive and binary
checksums and compatible protocol advertisements are verified before use.
Channel discovery is refreshed before cache reuse. Network errors remain
errors. For offline updates, choose provided files or the desktop’s included
build explicitly.

To bootstrap from a published bundle set, use its standalone `ctlup` for this
computer. Verify its checksum against the sidecar and run
`install --channel nightly`. SSH installation requires a publication or
supplied bundle whose target updater supports storage layout 7.

Within `ctlup/`, cached archives use a publication hash under
`cache/downloads`, extracted deployment builds use `cache/unpacked`, and
downloaded updater binaries use `cache/updaters`.
Automatic cleanup budgets compressed runtime downloads at 256 MiB, extracted
payloads at 512 MiB, and updater binaries at 128 MiB. Prepared extracted builds
hold shared leases and can temporarily exceed the budget until users release
them. Installed channel components, checkout builds and active payloads are separate
from deployment cache. Cleanup after installs and updates protects exact cached
recovery copies and matching updaters while their catalog records exist. An
explicit `ctlup prune --forget-missing` drops missing builds and clears any
selections that refer to them. Their cached copies then follow the usual cache
budgets. `ctlup prune --json` reports those IDs in `forgotten_missing_builds`.

## Remote deployment

For a published update, the desktop or `ctlup update --host NAME` resolves the
channel to one release and checks this computer’s cache separately for the
target’s updater and runtime archive. It can send either verified file over SSH.
For example, if only the runtime is cached on this computer, the host can fetch
`ctlup` and receive the runtime over SSH. The host can also reuse a matching
installed updater or verified archive. When it needs to download a file, it
tries `curl`, then `wget`. If the host cannot reach the release, lacks download
tools, or cannot use direct download, this computer
retrieves any missing files and uploads the same release. Integrity, account
identity and activation failures stop the update.

Provided and development builds travel from this computer over SSH. The remote
`ctlup` activates the verified build using the installation lock. The desktop
shows **Activated existing build** when the remote updater confirms reuse.
A changed same-ID payload fails verification.

Saved account identity is checked before mutation and the installed agent is
verified afterward. A failed post-install check reports whether activation may
have completed and leaves running services in place.

When a Unix host has a missing or recognized outdated agent, `ctl` prints a
`ctlup install` command for the selected host or SSH alias. Run it, then reconnect.
Whole-runtime installation and deployment support Windows x64 clients and SSH
targets as well as the supported Unix platforms. Windows ARM64 currently supports
the local desktop app commands above; ARM64 whole-runtime bundles are unavailable.

## Desktop component updates

Use **Update components…** in the command palette, About, or host settings.
Choose hosts and a channel, provided files, or an included offline build.
In a development app, select a Linux x64 or ARM64 SSH host and choose
**Build from checkout** to compile on this computer from the checkout used to
build the app and upload the result. Choose **Provided build files** to upload a
[previously packaged development build](local-bundles.md#build-a-local-linux-bundle).
When installing from the connection prompt, the dialog targets the SSH route
that needs the components. Progress identifies which computer is downloading,
which files travel over SSH, and when the remote host activates the build. Every
update installs a complete build. About’s bundle list shows the active native
build and offers local installation of another usable build. Explicitly installed
target components have durable channel registrations; other downloaded foreign
artifacts remain deployment cache entries.

## Inspect, restart and stop from the CLI

```sh
ctl components status
ctl components status --json
ctl components restart ctld --dry-run
ctl components restart ctld
ctl components restart ctmuxd
ctl components restart ctl-taskd
ctl -H work components status
ctl -H work components restart ctmuxd
ctl components stop ctld
ctl components stop ctmuxd
ctl components stop ctl-taskd
ctl -H work components stop ctmuxd
ctl --destination alice@server components stop ctmuxd
```

`status` compares each running owner's full build identity and protocol advertisements
with the installed replacement selected for this CLI. Release versions alone do not
identify builds or protocol compatibility. Human output shows a short source fingerprint;
`--json` preserves complete build and protocol metadata. Unknown or legacy metadata is
not treated as an up-to-date build. Missing automatically discovered replacements are shown as `not installed` and do not
fail status; the running owner is still inspected independently. An explicit missing
executable override or a damaged selected installation remains an error. Inspection
failures are reported for each component,
with a nonzero exit status, while successful rows remain visible.

Local ctld setup, status, startup, and restart use one shared Rust resolver. It
reports the selected source, executable, archive SHA-256 when available, and build
metadata independently from the running socket owner. `ctl setup --json` includes
this `selection`; `components status --json` includes `available_selection`.
An archive hash identifies the signed app bytes; a source fingerprint identifies
compiled inputs, and a checkout ID identifies its canonical path. None implies
that a development helper was rebuilt after the latest source edit.

Inspection verifies the selected candidate without installing embedded payloads,
changing shared selections, or starting a daemon. A selected checkout or embedded
app that still needs caching is shown as `pending preparation`; unknown build
metadata is not invented. Explicit `ctl setup` prepares this same selection.
`ctl ssh -G HOST` also uses passive selection when rendering a route's
`ProxyCommand`; rendering a future helper path does not prepare or launch it.
Startup prepares it only when an absent daemon must be launched. Existing owners
continue running independently until explicit activation. Remote status can
establish the saved host's SSH/VPN route, verifies its machine identity, and inspects
companions without starting those remote services. The remote agent is on demand;
its inspection process does not describe every active terminal transport.

`restart` verifies the selected replacement and pins the existing owner before showing
its impact and asking for confirmation. Use `--dry-run` to inspect the plan without
mutation, or `--yes` to explicitly approve it in scripts. `--json` does not imply consent.
Confirmation expires after 20 seconds; an expired plan must be prepared again. Restarts
use cooperative shutdown and verify the successor, without force-killing or automatically
retrying an uncertain mutation. An absent owner is not started by this command. ctld restart uses the same asynchronous
signed-helper discovery/preparation as normal CLI connections, preserving explicit
`CTLD_BIN` overrides. It checks that an owner exists before preparing a bundled helper;
status inspects that same selection without provisioning it. `ctl setup` never
switches from a selected checkout or explicit override to an unrelated release.

Restarting **ctmuxd ends all of its sessions and panes**, including other clients and
interactive tasks. Restarting ctld interrupts its clients and VPN connections. Taskd
refuses restart while tasks are active. Each local command addresses the CLI's selected
endpoint (`CTLD_SOCKET_PATH`, `CTMUX_RUNTIME_DIR`, or `CTL_TASKD_RUNTIME_DIR`), rather than
every daemon process on the machine. Remote restart currently supports only ctmuxd;
ctld and ctl-taskd are rejected before connecting.

Sync/import and update/install continue to preserve running owners. Restart is a separate,
explicit step after selecting and installing a build. `stop` pins an existing owner and uses cooperative shutdown without selecting,
installing or launching a replacement. It accepts the same `--yes`, `--dry-run`
and `--json` options as restart, with a 20-second confirmation window. Stopping
an absent local owner succeeds without starting it. Active tasks cause taskd to
refuse shutdown. Stopping ctmuxd ends all its sessions and panes; stopping ctld
interrupts its clients and VPNs. No command force-kills a daemon or retries an
uncertain shutdown. Later connections can start services again; stop does not
disable normal automatic startup.

Local stop reuses the shutdown phase of the existing lifecycle contracts.
Remote stop requires ctl-agent maintenance `1.2.4` (build 4), verifies the remote
machine identity and requires a separate confirmation. Remote stop supports only
ctmuxd; remote ctld and ctl-taskd are rejected before connecting. An older agent
must be updated before remote stop can be used. Existing restart and inspection
contracts remain supported.
