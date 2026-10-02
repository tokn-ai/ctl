# ctl-cli

Installs the `ctl` command for local and remote terminal sessions, managed tasks,
SSH, file copies, port forwards, and VPN connections.

## Install

Once the packages are published:

```sh
# macOS: install the matching signed helper from its published GitHub release.
cargo install --locked ctl-cli ctmuxd ctl-taskd
ctl setup

# Other Unix clients.
cargo install --locked ctl-cli ctld ctmuxd ctl-taskd
```

Cargo installs only the selected packages' executables. `ctld`, `ctmuxd`, and
`ctl-taskd` are companion daemons and must be installed separately from `ctl-cli`.
Keep their versions aligned. Terminal/task daemons should share the CLI's binary
directory; `ctl setup` installs the macOS helper under
`~/.tokn/ctl/components/ctld/` and switches its `current` symlink after verifying
the full signed and notarized app bundle. It never restarts an existing daemon.

For remote sessions and tasks, install `ctl-agent`, `ctmuxd`, and `ctl-taskd`
on the controlled machine. On macOS, Touch ID-protected credential storage
requires the signed, provisioned `ctld` helper; Cargo installation alone does
not supply its Keychain entitlement. Setup selects the release matching this
CLI's version and architecture and requires that release to be published.
`CTLD_BIN` remains an explicit override. Otherwise the desktop's bundled helper
has priority over an embedded CLI helper, then the managed installation, then a sibling
or `PATH` executable.

Official macOS CLI downloads carry the complete matching signed `ctld.app`
inside the `ctl` executable. They install it under the same managed directory
when a command needs the helper, or when you run `ctl setup`, without downloading
it. Running daemons and passive status reads are preserved. `CTLD_BIN` and the
desktop helper retain priority. A dedicated
[build/release command](https://github.com/tokn-ai/ctl/blob/main/docs/ci-bundles.md#build-a-cli-with-ctld-embedded)
compiles and bundles the helper; ordinary Cargo installation does not embed it.

## Use

```sh
ctl --help
ctl host list
ctl skill
ctl skill --list
```

The bundled skills and references are available without a daemon, connection,
or source checkout.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
