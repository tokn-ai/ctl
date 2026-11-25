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
runtime's `ctld` signature. Run `ctlup sign --version VERSION` to download, sign,
and notarize a tagged CI desktop release. Both reuse the existing build.
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
