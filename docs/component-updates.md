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

## One active installation

`ctlup` installs and updates complete builds. `ctl` and `ctmux` perform runtime
operations. The desktop uses the same Rust installation engine as `ctlup`.

Each account has one active build containing `ctl`, `ctmux`, `ctl-agent`,
`ctmuxd`, `ctl-taskd`, and `ctld`. Builds are immutable and verified against
their file checksums, build identity, target and shared protocol contracts.
Foreign-target binaries are never executed during download or import.

```text
~/.tokn/ctl/
  bin/ctlup                         # independent updater executable
  bin/ctl -> ../current/ctl          # stable command aliases
  bin/ctmux -> ../current/ctmux
  bin/ctl-agent -> ../current/ctl-agent
  current -> installs/<content-id>
  previous -> installs/<previous-id>
  installs/<content-id>/bundle.json
  installs/<content-id>/<executables>
  cache/                            # reusable deployment artifacts
```

On Windows x64, `%USERPROFILE%\.tokn\ctl\bin` contains native `.exe` launchers.
They resolve the active runtime or updater from the installation store. This
allows `ctlup` to install a new updater while the current process is running.

Add `~/.tokn/ctl/bin` to `PATH`. New managed terminal shells include it
automatically; existing shells keep their environment. Ordinary SSH login
configuration remains under the account owner’s control.

Activation atomically changes `current`. The previous complete build is kept
for rollback. Older installations remain only while live processes hold leases
on them; OS-held locks release after a crash. Installing never restarts daemons
or ends sessions. Existing agent channels use their original process until
reconnect. A terminal-daemon restart remains a separate confirmed action.

## Install, update and roll back

```sh
ctlup install
ctlup update
ctlup update --channel stable
ctlup update --channel nightly
ctlup update --pr 155
ctlup update --host work
ctlup update --hosts work,jump-a --local
ctlup update --destination my-ssh-alias
ctlup update --destination windows-alias --remote-platform windows
ctlup update --from /path/to/bundle-set --hosts work,jump-a --json
ctlup install --from target/debug --local-build --ctld-package /path/to/ctld-package
ctlup status --json
ctlup rollback
ctlup rollback --host work
ctlup prune
```

`install` and `update` share the same verification and activation transaction.
No host option means this account. With a remote target, `--local` also includes
this computer. Saved hosts reuse their existing route, connection method and
SSH authentication. A temporary destination uses OpenSSH configuration without
adding a saved host. Hosts are processed independently. A failure preserves
successful hosts and produces a nonzero exit status.

Windows SSH hosts use the platform saved in the connection method. For a
temporary Windows destination, supply `--remote-platform windows`. The server
uses the default Windows OpenSSH `cmd.exe` shell. Start a local Windows
installation by running the standalone x64 `ctlup` executable from the
publication.

An explicit channel is saved as this account’s preference after a successful
command. Otherwise automatic updates use that preference or the distribution’s
compiled default. Stable tracks tagged releases and Nightly tracks the latest
successfully published CI build from `main`. Nightly versions include the UTC
commit date, for example `0.1.2-nightly.20261110+g7f380d40f15e`.

Use `ctlup release list` to see the recent releases retained for your selected
channel. Each row shows the publication ID and marks the current release. Use
`ctlup release view` to inspect the current release, or pass a publication ID
from the list to inspect a specific build and its target compatibility.

```sh
ctlup release list --channel nightly
ctlup release view --channel nightly
ctlup release view PUBLICATION_ID --channel nightly --json
ctlup release list --channel stable
ctlup release list --channel dev
ctlup release build --pr PR_NUMBER
ctlup release view --pr 155
```

Both commands read publication metadata. `ctlup check` still checks the current
channel directly. Stable becomes available after CI publishes a complete bundle
set from a release tag, including a matching updater for each target.

The Dev list shows the 10 most recently updated PRs. An unexpired bundle for
the current commit is marked `built`. One for an earlier commit on the branch
is marked `outdated`. A branch without an unexpired bundle shows `no build`.
`release build --pr` uses the authenticated GitHub CLI (`gh`) to queue a build
for a branch in this repository. It reports an existing bundle or active build
for the same commit when available. `release view --pr` shows the PR and its
build status. `update --pr` uses `gh` to download the bundle for the current
commit. Use `--host` to target a remote machine. See
[install a temporary PR build](ci-bundles.md#install-a-temporary-pr-build).

In a development checkout, automatic Linux x64 remote updates build the working
tree. Use `--working-tree` to choose that source explicitly for Linux x64 or
ARM64 targets.

`--from` supplies an exact complete build or publisher bundle-set. Publisher
archives need their matching `bundle-set.json` or `bundle-set-windows.json`.
Foreign-target deployment also
needs that target’s separately checksummed updater sidecar. Native local builds
must include all six executables. There are no upload selections or agent-only
overlay installs. Old `ctl components` mutation commands direct users to `ctlup`.

On macOS, a local service installation requires the complete verified signed
`ctld.app` and receipt. `--ctld-package` supplies these for a native build.
The package remains intact inside the installation. Flat remote macOS bundles
do not replace an independently provisioned signed local helper.

## Install or update the desktop app

```sh
ctlup app install
ctlup app install --channel stable
ctlup app install --local
ctlup app update
ctlup app update --channel nightly
ctlup app update --local
```

These commands install on this computer. `--local` builds a native debug app
from the current source checkout, including its frontend and bundled daemons.
Run it inside a ctl checkout. A local build
preserves the saved publication channel and conflicts with `--channel`.
Install the checkout's frontend dependencies with `pnpm install --frozen-lockfile`
before building locally.

Windows supports native x64 (`x86_64-pc-windows-msvc`) and ARM64
(`aarch64-pc-windows-msvc`). Both use a portable, per-user directory rather than
MSI or NSIS installation; installing the app does not require administrator
access. Local builds require Node 24, pnpm, Rust's matching MSVC toolchain, the
Visual Studio C++ build tools and Windows SDK.

Local macOS builds sign the bundled `ctld.app` when a usable development
profile and signing identity are available, and fall back to an unsigned
helper when signing is unavailable.

Published installs use the same saved channel preference as runtime bundle
updates, with the distribution's compiled default when no preference is saved.
`--channel stable` or `--channel nightly` selects the published source and saves
that choice after a successful install or update.

Stable uses the desktop package attached to the matching tagged release.
Nightly uses authenticated GitHub CLI (`gh`) access to download the desktop
Actions artifact for the exact source revision selected by the Nightly channel.
If that build's desktop package is unavailable for this computer, the command
reports the missing package.

On macOS, launch the installed app with `open ~/.tokn/ctl/apps/ctmux.app`.
Quit ctmux before updating it. The installer prefers the signed, notarized
archive and uses the official unsigned archive when the release has no
signed manifest. On Linux, launch `~/.tokn/ctl/apps/ctmux.AppImage`.
On Windows, launch the executable from PowerShell:

```powershell
& "$env:USERPROFILE\.tokn\ctl\apps\ctmux\ctmux-app.exe"
```

Close ctmux and the local helpers running from its Windows app directory before
updating; the updater refuses to replace files that are in use. The portable ZIP contains
the desktop executable, `ctld.exe`, `ctmuxd.exe`, `ctl-taskd.exe`, resources and a
`portable.json` inventory binding every file's SHA-256 digest to the app version
and native target. Keep that directory together when moving a portable copy.
GUI launch requires Microsoft's
[WebView2 Evergreen Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).
`ctlup` installs the app files without installing WebView2; a missing runtime
must be installed separately before launch.

Windows releases also provide a standalone
`ctlup-<bundle>-<target>.exe`, its `.sha256` checksum and `ctlup-<target>.json`
metadata. Download the files for the native architecture from the same release,
or from the same Nightly desktop Actions artifact. Compare `Get-FileHash -Algorithm
SHA256` for the executable with the checksum before running it. The metadata
records the target, app version, bundle identity and exact source revision.
Run the downloaded executable with `app install` or `app update`; it can remain
outside the installed app directory.

For published packages, `ctlup` checks the release manifest and the package's
SHA-256 checksum before replacing the app. An update keeps the previous app at
the path printed by `ctlup`. On Windows, the previous directory is retained at
`~/.tokn/ctl/apps/.ctlup-ctmux/previous`. Windows replacement uses two directory
renames with a short gap when the app path is absent. It retains an interrupted
replacement at `.ctlup-ctmux/pending-previous`; the next `ctlup app install` or
`ctlup app update` restores that directory if the app is missing, or retains it
as the previous app if activation completed. This is recoverable replacement,
not an atomic directory exchange. The updater does not stop running apps or
helpers to make room. On macOS and Linux, independently running daemons and their
terminal sessions continue.
Desktop packages can become available after their matching runtime release.

On macOS, `pnpm ctld:build` from a checkout prepares a signed local helper
through `ctlup dev`. At the next daemon startup, the desktop prefers the
selected signed helper, then a compatible separately provisioned signed
helper, then its bundled helper. The desktop can use a fallback if a
development provisioning profile has expired. The app's own release
signing is independent of the helper selection.

## Native development builds

```sh
ctlup dev --workspace /path/to/checkout
ctlup dev --release
ctlup dev --profile custom --target-dir /path/to/build-output --json
```

`dev` builds the six native runtime components and the independent updater from
one checkout. On macOS it signs and verifies the complete `ctld.app` package.
Only a successful complete build becomes active; compilation, signing or
verification failure leaves the previous installation and running services
intact. `ctlup status` then reports that development installation. Foreign
targets are rejected because this command prepares the current computer.

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

If an older `ctlup` rejects dated Nightly versions, use the standalone `ctlup`
for this computer from a published Nightly bundle set. Verify the executable
against its SHA-256 sidecar, make it executable, and run it with
`update --channel nightly` to install the bundle and updater together.

Cached archives live under `cache/downloads`, extracted deployment builds under
`cache/components`, and standalone updater binaries under `cache/updaters`.
Automatic cleanup budgets compressed runtime downloads at 256 MiB, extracted
payloads at 512 MiB, and updater binaries at 128 MiB. Prepared extracted builds
hold shared leases and can temporarily exceed the budget until users release
them. Active and rollback installations are separate from deployment cache.

## Remote deployment

The client downloads and verifies files locally, then uploads through the
authenticated SSH route. The remote host needs no internet access. A small
bootstrap atomically prepares `bin/ctlup` when absent or changed. That updater
uses the same installation transaction and crash-releasing OS lock as a local
update. Verified identical installed builds are activated without reuploading
the runtime archive. A changed same-ID payload fails rather than being replaced.

Saved account identity is checked before mutation and the installed agent is
verified afterward. A failed post-install check reports that activation may
have completed; it does not retry, roll back or stop services automatically.

When a Unix host has a missing or recognized outdated agent, `ctl` prints a
`ctlup update` command for the selected host or SSH alias. Run it, then reconnect.
Whole-runtime installation and deployment support Windows x64 clients and SSH
targets as well as the supported Unix platforms. Windows ARM64 currently supports
the local desktop app commands above; ARM64 whole-runtime bundles are unavailable.

## Desktop and migration

Use **Update components…** in the command palette, About, or host settings.
Choose hosts and a channel, provided files, or an included offline build.
Development builds can also cross-build Linux x64 and ARM64 from the checkout.
The dialog reports preparation progress before SSH transfer begins. Every
update installs a complete build. About’s bundle list shows the active native
build and offers local installation of another usable build; foreign artifacts
are deployment cache entries, not independent upload profiles.

Existing verified complete installations and local selections are recognized
without mutation. The first successful explicit install migrates the previous
complete build into the new rollback store. Historical four-service installs
can be retained as rollback candidates; new installs require all six binaries.
Legacy trees remain intact because processes predating leases may still use
them. Host identity, saved settings and sessions are preserved.

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
