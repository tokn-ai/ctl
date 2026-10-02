# Release bundles and Apple signing

The `Desktop, control daemon, and remote-agent bundles` workflow builds remote
agent bundles and desktop packages for Linux and macOS, on Intel/AMD and ARM64.
Version tags also build standalone signed macOS `ctld.app` bundles and signed
macOS `ctl` executables with those complete helper bundles embedded. The workflow
creates or updates a draft release; it never publishes the release automatically.
Manually maintained release notes and attachments are preserved, and a published
release is left unchanged.

## Build selection and versions

- A push to `main` builds desktop and remote-agent development bundles. Missing
  Apple credentials allow unsigned macOS desktop development artifacts, with
  that limitation recorded in the draft notes.
- A `v<version>` tag builds desktop, remote-agent, and standalone `ctld` bundles.
  The tag must match the workspace Cargo version, Tauri app version, and frontend
  package version. Both macOS architectures require distribution signing.
- A manual run builds remote-agent bundles. `build_desktop` defaults to `true`;
  setting it to `false` supports the remote-only workflow used by agent syncing.
  `build_ctld=true` additionally builds standalone signed macOS helpers and
  requires the Apple credentials below.

Release bundle IDs are the version; development IDs append the source revision.
Draft releases on `main` may omit standalone helpers. Any build that uploads
helpers must include both architectures, with matching version, source revision,
and Apple Team ID. Version builds always require both helpers, both bundled CLI
architectures, and signed macOS
desktop packages. Publishing that draft makes the helper available to
`ctl setup` for that exact CLI version.

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

## Build a CLI with ctld embedded

For local macOS development, use the shared Tauri provisioning flow:

```sh
node scripts/dev/ctl-signed.mts --provision
# In Xcode, choose your team in Signing & Capabilities and build once.
node scripts/dev/ctl-signed.mts
target/ctl-dev/ctl setup
```

The build detects the native Rust target and workspace version, finds or
refreshes the provisioning profile, and chooses its matching Keychain
certificate. It compiles `ctld`, signs the complete app, embeds it in `ctl`, then
signs the CLI and atomically replaces the development output. It allows local
source changes and needs no notarization credentials. Its manifest binds the
helper's revision, source fingerprint, and dirty flag. Development bundles use
`~/.tokn/ctl/components/ctld/development/<archive-sha256>/`. They update the shared
architecture/API selection while leaving the release `current` symlink intact.
All standalone CLI builds, including ordinary Cargo builds, can reuse that
selected app. The runtime still checks signature, provisioning expiry,
certificate identity, source metadata against the app's own manifest, and API
compatibility. Rebuild after a profile expires. An existing compatible daemon is
never restarted automatically.

Local signing explicitly uses `--timestamp=none` for both the helper and CLI,
so an unavailable Apple timestamp service does not block development. Release
signing retains `--timestamp`; it fails if a secure timestamp cannot be obtained.

For distributable releases, use the following command.

On macOS, with a clean checkout, the matching Developer ID certificate installed
in Keychain, and these environment variables configured:

```sh
export CTLD_PROVISIONING_PROFILE=/path/to/ctld.provisionprofile
export APPLE_API_KEY_PATH=/path/to/AuthKey.p8
export APPLE_API_KEY=YOUR_KEY_ID
export APPLE_API_ISSUER=YOUR_ISSUER_ID

node scripts/ci/build-ctl-bundle.mts \
  aarch64-apple-darwin 0.1.0 "$(git rev-parse HEAD)" target/ctl-cli-assets
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

Setup uses the fixed `https://github.com/tokn-ai/ctl/releases/download/v<version>/`
source and approved GitHub HTTPS asset redirects. It installs the release matching
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
  selected/<target>-ctld12-lifecycle1-helper1 -> ../versions/<version>-<target>
```

Remote-agent `~/.tokn/ctl/versions/` and `current` remain independent. An existing
matching installation is verified and reused; setup does not overwrite a
version with different release contents. Release setup updates `current`; both
release and development setup atomically update the architecture/API selection.
A development selection points to `../development/<archive-sha256>` and does
not change `current`. Discovery follows this explicit selection instead of
sorting directories by hash or modification time. Setup never starts, stops,
or restarts a daemon. Existing compatible connections continue using their
running daemon until it is stopped explicitly.

An explicit `CTLD_BIN` executable override has highest priority. Otherwise the
standalone macOS CLI prefers a verified compatible selected managed app, then its
own bundled helper, then a nearby desktop bundle, sibling executable, or `PATH`.
The desktop continues to prefer its own bundled helper. Unsafe or invalid selected
installations produce a verification error. `ctl setup --json` prints the result with
`component`, `version`, `executable`, and `reused` fields.

A CLI built with the bundled release command prepares its embedded helper when
no compatible shared app is selected, before the sibling/`PATH` fallback.
It uses the same verified install transaction without downloading anything.
Preparation is lazy: starting an absent daemon, inspecting an explicit
replacement, or creating a fresh proxy route can require it. Passive status
reads and existing daemon/master reuse do not. Verification failures are
reported before launching SSH, rather than hidden behind a failed proxy command.

On other Unix platforms, install `ctld` from Cargo alongside the CLI. Cargo-built
macOS executables remain supported for development, but Cargo compilation does
not supply the official Developer ID signature or Keychain entitlement. npm and
PyPI installers can use these same signed helper assets in the future; this
workflow does not publish language-specific installers.
