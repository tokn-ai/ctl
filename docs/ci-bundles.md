# Release bundles and Apple signing

The `Desktop, control daemon, and remote-agent bundles` workflow builds remote
agent bundles and desktop packages for Linux and macOS, on Intel/AMD and ARM64.
Version tags also build standalone signed macOS `ctld.app` bundles and signed
macOS `ctl` executables with those complete helper bundles embedded. The workflow
automatically publishes component bundles to GitHub Pages. Version tags also
publish those bundles in a GitHub Release. Component publication does not wait
for desktop packaging, Apple signing, or notarization. Successful tagged desktop,
helper, and CLI builds append their verified packages to the matching published
release even when another target fails.
Manually maintained release notes and attachments are preserved.

## Build selection and versions

- A push to `main` builds desktop and remote-agent development bundles. Missing
  Apple credentials allow unsigned macOS desktop development artifacts, with
  that limitation recorded in the draft notes. Verified component bundles advance
  Nightly independently of desktop success.
- A `v<version>` tag builds desktop, remote-agent, and standalone `ctld` bundles.
  The tag must match the workspace Cargo version, Tauri app version, and frontend
  package version. macOS desktop builds use distribution signing when Apple
  credentials are configured and produce unsigned packages when credentials are
  missing. Component bundles publish first. Successful desktop, signed helper,
  and CLI targets append their assets later. A prerelease tag publishes a GitHub
  prerelease and versioned Pages archives without advancing the Stable channel.
- A manual run builds remote-agent bundles. `build_desktop` defaults to `true`;
  setting it to `false` supports the remote-only workflow used by agent syncing.
  `build_ctld=true` additionally builds standalone signed macOS helpers and
  requires the Apple credentials below.
- A manual run on a PR branch produces temporary Dev artifacts in Actions. Only
  `main` and version-tag runs can publish to Pages.

Main-branch draft refreshes still require complete desktop builds and all
requested helpers. Remote-only or failed refreshes leave that draft unchanged.

Release bundle IDs are the version. Nightly uses
`<version>-nightly.<YYYYMMDD>+g<revision>` with the UTC commit date and the
first 12 characters of the Git revision. A rerun keeps the same bundle ID.
Dev uses `<version>-dev.<revision>` for temporary branch builds.
Remote-agent publication requires all four targets because clients consume one
complete bundle set. Tagged desktop packages, standalone helpers, and bundled
CLIs are validated per available target. Each desktop target must contain every
package format for that platform. Helpers must match the version and source revision,
and available helper targets must agree on their Apple Team ID and protocols.
Each bundled CLI requires its matching same-target helper and archive checksum.
A helper can publish even if the later CLI build fails. On retry, CI restores the
verified published helper before uploading the helper artifact or building the
CLI, so the CLI embeds the same canonical helper bytes.

Unsigned macOS desktop release downloads add `-unsigned` after the target in
the DMG, app archive, checksum, and manifest filenames. Gatekeeper may block
first launch, and Touch ID-protected credential storage is unavailable. A later
successful signing run can append the signed desktop packages under their usual
filenames. Each published package retains its original checksum.

The standalone macOS helper and bundled CLI require distribution signing and
notarization. Missing Apple credentials fail those jobs while the successful
packages still publish. Adding a successful signed helper makes it available to
`ctl setup` for that exact CLI version and architecture.

### Install a temporary PR build

Queue the bundle workflow for a PR branch in this repository. The build command
uses authenticated `gh` and requires permission to run Actions. After CI
produces a bundle for the PR's current commit, use `ctlup` to view or install it.

```sh
ctlup release list --channel dev
ctlup release build --pr PR_NUMBER
ctlup release view --pr PR_NUMBER
ctlup update --pr PR_NUMBER --host HOST
```

The artifact expires after 30 days. The download includes the runtime archive
and updater for each target. To use a specific retained run, download it with
`gh run download RUN_ID -n ctl-agent-bundle-set -D ./bundle-set` and use
`ctlup update --from ./bundle-set`.

## Component channels and GitHub Pages

The public component source is
[`https://tokn-ai.dev/ctl/bundles/`](https://tokn-ai.dev/ctl/bundles/).
It contains each target's runtime archive, SHA-256 sidecar, standalone `ctlup`
executable and updater metadata, plus the complete bundle-set document.
New publications use schema 3. Previously published schema-2 sets remain usable.

Applications discover the two published channels at these URLs.

```text
https://tokn-ai.dev/ctl/bundles/channels/nightly.json
https://tokn-ai.dev/ctl/bundles/channels/stable.json
```

Each channel document uses schema 1.

```json
{
  "schema_version": 1,
  "channel": "nightly",
  "bundle_set_url": "https://tokn-ai.dev/ctl/bundles/sets/0.1.2-nightly.20261110+g0123456789ab-12345678-1/bundle-set.json"
}
```

Stable uses `"channel": "stable"`. The referenced immutable directory contains
`bundle-set.json` and all four target archives and their `.sha256` sidecars.
New publications also include these files for each target:

```text
ctlup-<bundle-id>-<target>
ctlup-<bundle-id>-<target>.sha256
ctlup-<target>.json
```

The updater's schema-1 manifest binds its version, source revision, target,
filename, byte size and SHA-256. Packaging strips and probes the actual updater
bytes before recording them. Its identity must match the runtime publication.
The updater stays outside the six-component runtime archive, so changing the
active runtime or rolling it back never replaces the installation manager.
Older publications without updater sidecars remain readable. A partial updater
set or a checksum mismatch blocks assembly/publication; CI emits all four targets.
`pnpm bundles:sync` verifies and stages these files for desktop packaging only.
It does not import an account cache, select a build or activate an installation;
ordinary updates populate their deployment cache on demand.
Archive names remain bare sibling filenames in the bundle set. Its existing
checksums, archived manifests, component metadata, and protocol compatibility
validation remain authoritative; channel discovery adds no parallel hash format.

Nightly tracks the latest **successfully published** build from `main`.
A later main commit that fails to build leaves the previous publication available.
Run number and attempt fences prevent a delayed older run from replacing a newer
successful publication. Stable advances by semantic release version. Lower
release tags and prereleases leave its pointer unchanged. Prerelease archives remain
available through their retained versioned Pages directory and GitHub prerelease.

The Pages publisher retires the legacy `dev.json` alias. The Dev list in `ctlup`
uses PR and Actions metadata from GitHub.

CI builds select a default update channel explicitly via `CTL_BUNDLE_CHANNEL`.
Ordinary version tags select Stable. All other builds select Nightly as their
default published update feed, including Dev artifacts and prereleases. Pages
publication IDs append the Actions run ID and attempt to the bundle ID, so reruns
never overwrite an existing directory.

The Pages job serializes publication across main and tags with a queued concurrency
group. It restores the complete last successful site, verifies retained component
sets, stages every new archive and updater, then writes channel pointers and
uploads one complete Pages artifact. A separate 90-day Actions snapshot records
the full site. Restoration requires the exact
attempt's successful Pages deployment step, even if a later desktop job failed or
the overall workflow is still running. Failed deployment candidates are ignored.
After snapshot expiry, restoration downloads and verifies the managed published
HTTPS catalog, archives and updater sidecars. Network errors, corrupt state, and an existing
unmanaged site abort publication rather than initialize an empty replacement.

Pages retains up to five Nightly publications, the current Stable publication,
and the newest other tagged publication. The current Nightly and Stable sets and
the newest other tag are pinned. If the staged site would exceed 900 MiB, older
unselected Nightly publications are removed oldest first until it fits. If pinned
publications alone exceed the budget, publication fails without changing the
deployed site. Retention changes occur only in a complete replacement site;
historical tagged archives also remain in GitHub Releases.
`ctlup release list` and `ctlup release view` use the retained Pages catalog for
Nightly and Stable. Older retained Nightly builds keep their original bundle IDs.

Version publication validates all component archives before creating or updating
a release. Existing main-draft development packages are removed before the tag
release becomes public. Published assets are immutable: retries compare GitHub's
asset SHA-256 digest and size, and mismatching bytes or manual-name collisions
fail. Appending packages revalidates each available target and the complete
published component manifest; it cannot replace another build's assets.
Tag reruns download and verify the first published component set as canonical,
then reuse it, including already-published updater bytes, for Pages and desktop
resources. A retry may append missing updater sidecars only after checking the
tag's exact source revision; existing sidecars cannot be replaced. Published package targets are
likewise reused on rerun, accommodating timestamp/signing differences without
replacing published files while allowing missing targets to be appended.
An interrupted upload must retain every already-published file's digest and size;
conflicting rebuilt bytes fail explicitly instead of mixing package generations.
Resume such an upload with the original run's artifacts. An existing empty
release can receive its first verified component set after the tag's source
revision is checked.

### Initial repository setup

In repository **Settings → Pages → Build and deployment**, select **GitHub
Actions** as the source. Allow main and version tags in the `github-pages`
environment's deployment rules. The workflow needs its declared `pages: write`,
`id-token: write`, Actions/deployment read access, and release `contents: write`
permissions. It does not create Pages settings or enable another publishing
source automatically. The fixed HTTPS `tokn-ai.dev/ctl` URL uses the organization's
existing GitHub Pages domain. Other domains or unrelated existing content must be
reconciled explicitly first. Initial deployment permits current in-progress jobs
and earlier failed jobs when no site exists; any prior successful deployment
without its managed catalog stops publication rather than resetting the site.
Appending desktop assets requires GitHub release immutability to remain disabled
for these releases. If immutable releases are enabled, component publication still
succeeds but GitHub prohibits later desktop attachments; the publisher reports
that policy without changing it.

Repository tests use fake GitHub/fetch operations and real verified tar archives.
They cover complete-site restoration, both channels, stale runs, prereleases,
retention, corruption, failed uploads/deployments, and append-only desktop assets.
Running these tests does not deploy Pages, change settings, or publish a release.

Remote bundle schema 3 ships six executables for each target: `ctl-agent`,
`ctmuxd`, `ctl-taskd`, `ctld`, `ctl`, and `ctmux`. Packaging runs
`--component-info` on each binary. The archive manifest binds its build identity
and supported protocol versions to the binary's checksum. Assembly verifies the
archive before copying the advertisements into the bundle-set document. Cache
reuse checks both documents and the contracts shared by the clients and services.

Schema 2 remains readable with its original four service binaries and published
protocol advertisements. Schema 1 retains its original checksum and exact-source
revision requirements. The managed store uses a separate `bundle.json` schema.
Managed schema 2 identifies a complete six-executable build, and managed schema 1
continues to identify its four-executable predecessors.

A full installation uses one immutable payload under
`~/.tokn/ctl/installs/<content-id>` and atomically switches
`~/.tokn/ctl/current`. Stable command entry points live in `~/.tokn/ctl/bin`;
add that directory to `PATH` for an ordinary SSH login. Managed terminal sessions
and ctl SSH gateway launches add it automatically. `ctlup` is a separate executable
at `~/.tokn/ctl/bin/ctlup`, while runtime command aliases resolve through `current`.
The installer leaves login shell configuration to the account owner.

Clients obtain and verify the target updater locally. When it is absent or its
checksum differs on the receiving account, they bootstrap it over authenticated
SSH without requiring remote internet access. The updater verifies a retained
runtime before deciding whether an archive upload is needed, then applies the
same installation transaction as a local `ctlup install`. The previous complete
build is retained for rollback, and process-held leases preserve builds still
used by running clients or daemons. Updates never restart those processes.

Component metadata probes execute the packaged bytes, including Intel macOS
executables through Rosetta on ARM runners. The bundle workflow sets
`CTL_COMPONENT_PROBE_TIMEOUT_MS=30000` to allow cold startup; standalone packaging
defaults to five seconds. Probes retain the 16 KiB output limit and fail without
retrying a crashed executable or substituting metadata from another build.
Failures include the target, runner architecture, elapsed time, configured
deadline, exit code, signal, and bounded stderr. Successful probes taking more
than five seconds also report their timing on stderr.

## Apple distribution credentials

Configure these repository or organization secrets and variables:

| Setting | Kind | Value |
| --- | --- | --- |
| `APPLE_CERTIFICATE` | Secret | Base64-encoded `.p12` containing a Developer ID Application certificate and private key |
| `APPLE_CERTIFICATE_PASSWORD` | Secret | Password for that `.p12` |
| `APPLE_CTLD_PROVISIONING_PROFILE` | Secret | Base64-encoded Developer ID distribution provisioning profile for `dev.tokn-ai.ctl.ctld` |
| `APPLE_API_KEY_CONTENT` | Secret | Contents of the App Store Connect API key `.p8` file used by notarytool |
| `APPLE_API_KEY` | Variable | API key ID |
| `APPLE_API_ISSUER` | Variable | API issuer ID |

The helper's profile must authorize the signing certificate and exact app/team
identity, enable `ProvisionsAllDevices`, omit a device list, and disallow
`get-task-allow`. The Team ID must contain ten uppercase letters or digits.
An Apple Development certificate or development provisioning profile is not
accepted for a standalone release. Expired distribution profiles also fail.

`package-ctld-app.sh` stages the complete bundle, embeds the profile, and signs
with the hardened runtime and a secure timestamp. `package-ctld-bundle.mts`
checks the Developer ID certificate chain, app/team identity, signed
entitlements, version, and executable architecture. It submits a ZIP to Apple's
notary service, requires an accepted result, staples the ticket, and checks both
the signature and Gatekeeper assessment. Signing, notarization, or assessment
failures stop the build; no unsigned standalone release is substituted.

Repository script tests use fake signing/notarization operations and real tar
archives. On macOS, native checks also exercise plist parsing and requirement
compilation. They do not import signing secrets or submit software to Apple.
Signed release assets require a successful signing and notarization run.

## Standalone helper release assets

Each macOS target produces these assets:

```text
ctld-<target>.json
ctld-<bundle-id>-<target>.app.tar.gz
ctld-<bundle-id>-<target>.app.tar.gz.sha256
```

Targets are `x86_64-apple-darwin` and `aarch64-apple-darwin`. The archive root is
`ctld.app/`, including its executable modes, embedded provisioning profile,
signature, and ordinary-file stapled ticket. Portable tar archives omit AppleDouble
metadata and contain only files and directories. Packaging verifies the
extracted archive again with codesign, stapler, and Gatekeeper before creating
the manifest.

The manifest records schema version 1, component `ctld`, app version, bundle ID,
source Git revision, target, bundle identifier `dev.tokn-ai.ctl.ctld`, Apple Team
ID, `signing_mode: "signed"`, `notarized: true`, archive filename, SHA-256 hash,
and archive size. The draft updater validates the complete asset set and each
checksum before changing release attachments.

## Native development installations

For local macOS CLI and GUI development, run the shared provisioning commands
from the repository root:

```sh
pnpm ctld:provision
# In Xcode, choose your team in Signing & Capabilities and build once.
pnpm ctld:build
~/.tokn/ctl/bin/ctl passwords
~/.tokn/ctl/bin/ctlup status --json
```

For GUI development, `pnpm desktop:dev` delegates native preparation to
`ctlup dev` on macOS and Linux. On Unix, `pnpm daemons:build` also uses that
complete-build path; Linux requires no Apple provisioning. The macOS worktree
daemon survives app reloads, and running `ctld:build` first is unnecessary.
The macOS-only `pnpm ctld:build` bootstraps the same manager and delegates to the
same complete-build path for CLI development.

The manager compiles all six runtime commands and `ctlup` from the working tree,
snapshots Cargo's actual outputs, then strips and verifies those copies. On
macOS it finds or refreshes the provisioning profile, chooses its matching
Keychain certificate, and signs the copied `ctld.app`. The resulting receipt
binds its revision, source fingerprint, dirty flag, and protocol metadata.
The CLI and GUI can remain unsigned; no notarization credentials are needed.

Successful preparation installs one immutable build under
`~/.tokn/ctl/installs/` and activates it through the same selection used by
`ctlup update`. Stable commands live in `~/.tokn/ctl/bin`. Cargo profiles and
configured target directories are preserved; compilation caches under
`<target-dir>/ctl-dev/<checkout-id>/cargo` keep worktrees separate. Launchers
hold an installation lease while using its exact executable paths. Active,
previous, and running builds remain available after Cargo outputs are cleaned.

The runtime checks signature, provisioning expiry, certificate identity, source
metadata, and API compatibility. Rebuild after a profile expires. Preparation
preserves existing daemons and sessions; restarting an owner is a separate
action. See [local development builds](local-bundles.md) for direct `ctlup dev`
usage and custom Cargo profiles.

Local component signing explicitly uses `--timestamp=none`,
so an unavailable Apple timestamp service does not block development. Release
signing retains `--timestamp`; it fails if a secure timestamp cannot be obtained.

## Build a CLI with ctld embedded

For distributable releases, use the following command.

On macOS, with a clean checkout, the matching Developer ID certificate installed
in Keychain, and these environment variables configured:

```sh
export CTLD_PROVISIONING_PROFILE=/path/to/ctld.provisionprofile
export APPLE_API_KEY_PATH=/path/to/AuthKey.p8
export APPLE_API_KEY=YOUR_KEY_ID
export APPLE_API_ISSUER=YOUR_ISSUER_ID

node scripts/ci/build-ctl-bundle.mts \
  aarch64-apple-darwin 0.1.3 "$(git rev-parse HEAD)" target/ctl-cli-assets
```

Install the requested Rust target first. The command builds `ctld`, signs its
complete app bundle, notarizes and staples it, then compiles `ctl` with the final
manifest and archive embedded. It signs and notarizes the resulting CLI and
checks the transported executable before creating its archive. Both source
packages must match the supplied version and revision. No Apple credentials or
network work is placed in a Cargo build script.

CI reuses the helper assets it just built by passing their directory as a fifth
argument and `CTLD_SIGNING_IDENTITY_OUTPUT` as the certificate fingerprint file
emitted by `package-ctld-app.sh`. Reused archives are snapshotted, checked for
safe file/directory entries, and verified with codesign, stapler, and Gatekeeper
before embedding. The CLI release manifest binds its helper hash and Apple team
to the matching standalone helper release.

Each target adds:

```text
ctl-cli-<target>.json
ctl-<version>-<target>.tar.gz
ctl-<version>-<target>.tar.gz.sha256
```

The archive contains one `ctl` executable. Cargo's `CTL_BUNDLED_CTLD_DIR` build
input is used by the dedicated build scripts; ordinary Cargo builds and
`cargo install ctl-cli` contain no embedded helper. Generated bytes stay in
`OUT_DIR`, so Cargo source packages remain free of release binaries. This
pipeline currently supports the two macOS targets; other platforms retain
separate daemon installation.

The embedded app retains its profile, signature, and stapled ticket. Apple does
not support stapling a standalone command-line executable, so the CLI itself
uses online notarization verification; setup needs no helper download after the
CLI can run. Apple recommends codesign's notarization requirement for this type
of code; see [All About Notarization](https://developer.apple.com/videos/play/wwdc2019/703/).
No real signing or notarization occurs in repository tests.

## Per-user installation

After publishing the crates and matching signed GitHub release, on macOS:

```sh
cargo install --locked ctl-cli ctmuxd ctl-taskd
ctl setup
```

Setup prepares the helper selected by the same resolver as status, startup, and
restart. A selected checkout, bundled app, shared installation, or `CTLD_BIN`
override is reused or prepared before considering a download. With no eligible
helper, setup uses the fixed
`https://github.com/tokn-ai/ctl/releases/download/v<version>/` source and approved
GitHub HTTPS asset redirects. It installs the release matching
the CLI's version and Mac architecture, rather than resolving the latest release
or installing a draft. It trusts that repository's HTTPS manifest for the
publisher Team ID and requires an Apple Developer ID signature for that team
and `dev.tokn-ai.ctl.ctld`; there is no separately compiled vendor Team ID pin.
Archive size/hash, distribution profile, and notarization are checked before
executing the helper's metadata query. Its build/protocol identity is then
verified against the installation manifest before selection. Discovery can
reuse another release or development build when its native target and `ctld`,
`ctld_lifecycle`, and `ctld_helper` APIs match the CLI's requirements; its release
version, revision, and fingerprint need not equal the CLI's.

The full signed bundle is installed without `sudo`:

```text
~/.tokn/ctl/components/ctld/
  versions/<version>-<target>/ctld.app/
  development/<archive-sha256>/ctld.app/
  current -> versions/<version>-<target>
  selected/<target>-ctld1-lifecycle1-helper1 -> ../versions/<version>-<target>
```

The runtime installation's `~/.tokn/ctl/current` pointer remains independent of
this signed-helper package store. A complete local installation containing a
verified helper takes precedence; otherwise existing signed-helper discovery
continues. An existing
matching installation is verified and reused; setup does not overwrite a
version with different release contents. Downloaded official releases update
`current` and atomically update the architecture/API selection. A separately
selected development installation points to `../development/<archive-sha256>`
and does not change `current`. Preparing a checkout or embedded app only publishes
its immutable cache entry; shared selections stay unchanged, including for signed
release payloads. Discovery follows the explicit shared selection instead of
sorting directories by hash or modification time. Setup never starts, stops,
or restarts a daemon. Existing compatible connections continue using their
running daemon until it is stopped explicitly.

An explicit `CTLD_BIN` executable override has highest priority. Ordinary macOS debug
CLI and GUI builds first resolve this checkout's provisioned helper under its
target directory, then verify and pin it under
`~/.tokn/ctl/components/ctld/development/<archive-sha256>/` without changing global
selections. With no compatible
local helper, a standalone macOS CLI prefers a
verified compatible selected managed app supporting the requested operation, then its
own bundled helper, then a nearby desktop bundle. Without an eligible signed
app, macOS selects an official release awaiting explicit setup instead of an
unsigned sibling or `PATH` executable. Use `CTLD_BIN` for an explicitly chosen
manual helper; other Unix platforms retain sibling/`PATH` discovery. Packaged
desktop releases prefer their own bundled helper after an explicit complete-bundle
selection. Unsafe or invalid selected
installations produce a verification error. `ctl setup --json` retains
`component`, `version`, `executable`, and `reused`, and adds a `selection` with
`source`, `executable`, verified build `info`, `archive_sha256` when available,
and `installed`. Status returns the same selection as `available_selection`.
Inspection does not publish a cache entry or change shared selections; an embedded
or checkout candidate awaiting preparation remains visible with `installed: false`
and may have unknown build metadata until verification can complete. Setup and
startup preparation use that candidate, rather than selecting an unrelated
release. Installing never replaces the process already serving a socket.

A CLI built with the bundled release command prepares its embedded helper when
no compatible shared app is selected, before considering a nearby desktop app.
It uses the same verified install transaction without downloading anything.
Preparation is lazy: starting an absent daemon, preparing an explicit restart,
or creating a fresh proxy route can require it. Passive status
reads and existing daemon/master reuse do not. Verification failures are
reported before launching SSH, rather than hidden behind a failed proxy command.

On other Unix platforms, install `ctld` from Cargo alongside the CLI. Cargo-built
macOS executables remain supported for development, but Cargo compilation does
not supply the official Developer ID signature or Keychain entitlement. npm and
PyPI installers can use these same signed helper assets in the future; this
workflow does not publish language-specific installers.
