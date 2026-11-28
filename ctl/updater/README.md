# ctlup

`ctlup install` selects a source for a complete runtime bundle or the desktop
app. `ctlup update` follows the channel recorded by the selected installation.
Use `--host` for a saved host and `--ssh` for an SSH address or alias. Each
profile stores installed payloads and cache files under its `ctlup/` directory.

```sh
cargo install --locked ctlup
ctlup --help
```

For installation layouts and release channels, see the
[component update guide](https://github.com/tokn-ai/ctl/blob/main/docs/component-updates.md).

On macOS, run `ctlup sign` from a ctl checkout to renew the active development
runtime's `ctld` signature. Use `ctlup sign --channel nightly` or
`ctlup sign --version BUILD_ID` to sign a retained desktop archive from the
local cache. Install the desired desktop first, for example with
`ctlup install app --channel nightly --ad-hoc`. Without an active local
development runtime, bare `ctlup sign` selects the active cached desktop.

On macOS, `install` and `update` verify published signatures and sign unsigned
builds with Developer ID by default. Pass `--provision` to use an Apple
Development identity and a matching Xcode profile. `ctlup` remembers the chosen
Apple Team for future signing. Pass `--ad-hoc` to install without Apple
credentials. An ad hoc `ctld` cannot use the protected Keychain or Touch ID.
Updates reuse the installation's signing mode. `ctlup sign` accepts the same
flags for a cached desktop build.
[Release signing guide](https://github.com/tokn-ai/ctl/blob/main/docs/ci-bundles.md#sign-a-ci-built-macos-desktop-release-locally)

Use `ctlup ctream install` and `ctlup ctream run` for the separate ctream
viewer. `ctlup ctream update` replaces the active selection, and
`ctlup ctream status` shows the installed paths under
`$CTL_DATA_DIR/ctlup/store/ctream/` (`~/.tokn/ctl/ctlup/store/ctream/` by default).
With `--json`, `build_id` gives the directory name and `id` gives the full
content identifier.
Run a fresh install after upgrading from the old `ctream/installs/` layout.
Pass `--from` with a ctream workflow ZIP for an offline install or update.
Run `ctlup dev --ctream` from a checkout to build and activate development tools.
Use `--agent-target` to include a freshly built remote agent.
