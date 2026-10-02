# Release bundles and Apple signing

The `Desktop, control daemon, and remote-agent bundles` workflow builds remote
agent bundles and desktop packages for Linux and macOS, on Intel/AMD and ARM64.
Version tags also build standalone signed macOS `ctld.app` bundles. The workflow
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
and Apple Team ID. Version builds always require both helpers and signed macOS
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
Archive size/hash, distribution profile, notarization, and build/protocol identity
are checked before downloaded code is executed or selected.

The full signed bundle is installed without `sudo`:

```text
~/.tokn/ctl/components/ctld/
  versions/<version>-<target>/ctld.app/
  current -> versions/<version>-<target>
```

Remote-agent `~/.tokn/ctl/versions/` and `current` remain independent. An existing
matching installation is verified and reused; setup does not overwrite a
version with different release contents. `current` is selected atomically, and
setup never starts, stops, or restarts a daemon. Existing connections continue
using their running daemon until it is stopped explicitly.

An explicit `CTLD_BIN` executable override has highest priority. Otherwise macOS
prefers the desktop bundle's helper, then the managed installation, then a
sibling executable or `PATH`. An invalid managed selection fails rather than
falling back to a loose executable. `ctl setup --json` prints the result with
`component`, `version`, `executable`, and `reused` fields.

On other Unix platforms, install `ctld` from Cargo alongside the CLI. Cargo-built
macOS executables remain supported for development, but Cargo compilation does
not supply the official Developer ID signature or Keychain entitlement. npm and
PyPI installers can use these same signed helper assets in the future; this
workflow does not publish language-specific installers.
