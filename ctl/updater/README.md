# ctlup

`ctlup` installs, updates, and rolls back complete ctl and ctmux builds. It
manages release channels and target bundles for local and SSH destinations.

```sh
cargo install --locked ctlup
ctlup --help
```

For installation layouts and release channels, see the
[component update guide](https://github.com/tokn-ai/ctl/blob/main/docs/component-updates.md).

Use `ctlup ctream install` and `ctlup ctream run` for the separate ctream
viewer. `ctlup ctream update` replaces the active selection, and
`ctlup ctream status` shows the installed paths under `~/.tokn/ctl/ctream`.
Pass `--from` with a ctream workflow ZIP for an offline install or update.
Run `ctlup dev --ctream` from a checkout to build and activate development tools.
Use `--agent-target` to include a freshly built remote agent.
