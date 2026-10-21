# Release bundles and Apple signing

The `Desktop, control daemon, and remote-agent bundles` workflow builds remote
agent bundles and desktop packages for Linux and macOS, on Intel/AMD and ARM64.
Version tags also build standalone signed macOS `ctld.app` bundles and signed
macOS `ctl` executables with those complete helper bundles embedded. The workflow
automatically publishes component bundles to GitHub Pages. Version tags also
publish those bundles in a GitHub Release. Component publication does not wait
for desktop packaging, Apple signing, or notarization. Successful tagged desktop
builds append their verified packages to the matching published release.
Manually maintained release notes and attachments are preserved.

## Build selection and versions

- A push to `main` builds desktop and remote-agent development bundles. Missing
  Apple credentials allow unsigned macOS desktop development artifacts, with
  that limitation recorded in the draft notes. Verified component bundles advance
  the Dev channel independently of desktop success.
- A `v<version>` tag builds desktop, remote-agent, and standalone `ctld` bundles.
  The tag must match the workspace Cargo version, Tauri app version, and frontend
  package version. Both macOS architectures require distribution signing.
  Component bundles publish first; successful signed desktop/helper builds append
  their assets later. A prerelease tag publishes a GitHub prerelease and versioned
  Pages archives without advancing the Stable channel.
- A manual run builds remote-agent bundles. `build_desktop` defaults to `true`;
  setting it to `false` supports the remote-only workflow used by agent syncing.
  `build_ctld=true` additionally builds standalone signed macOS helpers and
  requires the Apple credentials below.

Release bundle IDs are the version; development IDs append the source revision.
Draft releases on `main` may omit standalone helpers. Any build that uploads
helpers must include both architectures, with matching version, source revision,
and Apple Team ID. Version builds always require both helpers, both bundled CLI
architectures, and signed macOS
desktop packages before appending desktop assets. The independently published
component release can exist while those packages are still building or failed.
Adding successful signed helper assets makes them available to `ctl setup` for
that exact CLI version.

## Component channels and GitHub Pages

The public component source is
[`https://tokn-ai.dev/ctl/bundles/`](https://tokn-ai.dev/ctl/bundles/).
It contains the actual archives and checksum sidecars, alongside their existing
schema 2 bundle sets. A catalog alone is insufficient for installation.

Applications discover channels through:

```text
https://tokn-ai.dev/ctl/bundles/channels/dev.json
https://tokn-ai.dev/ctl/bundles/channels/stable.json
```

Each channel document uses schema 1:

```json
{
  "schema_version": 1,
  "channel": "dev",
  "bundle_set_url": "https://tokn-ai.dev/ctl/bundles/sets/0.1.1-dev.0123456789ab-12345678-1/bundle-set.json"
}
```

Stable uses `"channel": "stable"`. The referenced immutable directory contains
`bundle-set.json` and all four target archives and their `.sha256` sidecars.
Archive names remain bare sibling filenames in the bundle set. Its existing
checksums, archived manifests, component metadata, and protocol compatibility
validation remain authoritative; channel discovery adds no parallel hash format.

Dev tracks the newest **successfully published** main build. A later main commit
that fails to build does not prevent an earlier successful build from publishing.
Run number and attempt fences prevent a delayed older run from replacing a newer
successful publication. Stable advances by semantic release version; a lower
release tag or any prerelease cannot move it backward. Prerelease archives remain
available through their retained versioned Pages directory and GitHub prerelease.

Builds explicitly set `CTL_BUNDLE_CHANNEL`: `dev` on main and prerelease tags,
`stable` on ordinary version tags. Channel choice does not depend on whether Cargo
uses a debug or release profile. Publication IDs append the Actions run ID and
attempt to the bundle ID, so reruns never overwrite an existing directory.

The Pages job serializes publication across main and tags with a queued concurrency
group. It restores the complete last successful site, verifies retained component
sets, stages every new archive, then writes channel pointers and uploads one full
Pages artifact. Both channels are included in every replacement. A separate
90-day Actions snapshot records the full site. Restoration requires the exact
attempt's successful Pages deployment step, even if a later desktop job failed or
the overall workflow is still running. Failed deployment candidates are ignored.
After snapshot expiry, restoration downloads and verifies the managed published
HTTPS catalog and archives. Network errors, corrupt state, and an existing
unmanaged site abort publication rather than initialize an empty replacement.

Pages retains five Dev publications, the current Stable publication, and the
newest other tagged publication. Older Dev directories are removed only when a
complete replacement site is staged; several generations remain available for
downloads using a recently cached channel document. Historical tagged archives
remain in GitHub Releases. A 900 MiB staged-site limit leaves room below the Pages
site limit; exceeding it fails without changing the deployed site.

Version publication validates all component archives before creating or updating
a release. Existing main-draft development packages are removed before the tag
release becomes public. Published assets are immutable: retries compare GitHub's
asset SHA-256 digest and size, and mismatching bytes or manual-name collisions
fail. Desktop appending revalidates the complete signed package/helper set and
the published component manifest; it cannot replace another build's assets.
Tag reruns download and verify the first published component set as canonical,
then reuse it for Pages and desktop resources. A complete published desktop set
is likewise reused on rerun, accommodating timestamp/signing differences without
replacing published files. An interrupted, incomplete desktop upload with
conflicting rebuilt bytes fails explicitly instead of mixing package generations;
resume it with the original run's artifacts.

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

Remote bundle schema 2 captures `--component-info` from each packaged executable,
including explicit published protocol support and companion client contracts.
The archived manifest binds those advertisements to binary checksums; assembly
copies them into each target's bundle-set entry after verifying the archive and
its identity. CLI cache reuse validates both documents and can select a compatible
bundle across product releases. Signing and version selection for `ctld.app`
remain separate from this remote bundle schema.

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
compilation. They do not import signing secrets or submit software to Apple. A distributable
release still needs a successful credentialed CI signing/notarization run.

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

## Signed ctld for local development

For local macOS CLI and GUI development, run the shared provisioning commands
from the repository root:

```sh
pnpm ctld:provision
# In Xcode, choose your team in Signing & Capabilities and build once.
pnpm ctld:build
cargo run -p ctl-cli -- passwords
```

For GUI development, `pnpm desktop:dev` automatically builds and stages signed
helpers on macOS. Its managed worktree daemon survives app reloads; running
`ctld:build` first is unnecessary. The standard Tauri command can also launch a
direct debug GUI build that discovers the same selected helper as the CLI.

The `ctld:build` command detects the native Rust target and workspace version, finds or
refreshes the provisioning profile, and chooses its matching Keychain
certificate. The component command compiles `ctld`, signs the complete app,
and atomically publishes its checkout selection under
`<target-dir>/ctl-dev/helpers/<checkout-id>/`. Ordinary macOS debug CLI and GUI builds
copy that app into the immutable managed development cache and verify the copy
before launching it. Both consumers can remain unsigned. A running daemon and
its askpass, identity-agent, and proxy children therefore remain executable after
Cargo build outputs are cleaned. Cargo's
configured target directory and canonical checkout identity determine discovery,
independently of the current directory. Worktrees sharing a target directory have
separate selections. Repeat this preparation after helper changes; ordinary CLI
edits need only a Cargo rebuild.

One `pnpm ctld:build` output supplies both consumers; no development CLI
embedding or signing step is needed. It allows local source changes and needs
no notarization credentials. Its receipt binds the helper's revision, source
fingerprint, and dirty flag. Preparation leaves shared component selections and
running services unchanged.
The runtime still checks signature, provisioning expiry,
certificate identity, source metadata against the app's own manifest, and API
compatibility. Rebuild after a profile expires. An existing compatible daemon is
never restarted automatically.

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
  aarch64-apple-darwin 0.1.1 "$(git rev-parse HEAD)" target/ctl-cli-assets
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

Remote-agent `~/.tokn/ctl/versions/` and `current` remain independent. An existing
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
