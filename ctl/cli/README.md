# ctl-cli

Installs the `ctl` command for local and remote terminal sessions, managed tasks,
SSH, file copies, port forwards, and VPN connections.

## Install

Once the packages are published:

```sh
cargo install --locked ctl-cli ctld ctmuxd ctl-taskd
```

Cargo installs only the selected packages' executables. `ctld`, `ctmuxd`, and
`ctl-taskd` are companion daemons and must be installed separately from `ctl-cli`.
Keep their versions aligned and install them in the same binary directory.

For remote sessions and tasks, install `ctl-agent`, `ctmuxd`, and `ctl-taskd`
on the controlled machine. On macOS, Touch ID-protected credential storage
requires the signed, provisioned `ctld` helper; Cargo installation alone does
not supply its Keychain entitlement.

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
