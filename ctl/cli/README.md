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
ctl vpn create
ctl vpn list
ctl vpn start NAME_OR_ID
ctl vpn stop NAME_OR_ID
ctl skill
ctl skill --list
```

`vpn create` opens a questionnaire and saves an OpenConnect or Tailscale profile
without starting a daemon or container. Password input is masked. `vpn list`
combines saved profiles from `~/.tokn/ctl/vpns.json` (or
`CTL_VPNS_PATH`) with runtime connections and their current SOCKS5 endpoints. It
never starts a daemon. Saved profiles stay visible as disconnected when inventory
is complete, or unavailable when it cannot be checked. `vpn start` connects a
saved profile and recreates its container when missing. Supply its exact stable
ID or unique exact name. `vpn stop` also accepts runtime VPN IDs. Omitting the
start selector opens a picker in an interactive terminal; stop does the same
with a current daemon. An older daemon uses its untargeted single-connection stop
directly. Scripts must supply a start selector; an untargeted stop requires at most one local
connection. All five commands support `--json`; create remains interactive and
returns saved metadata, while list returns a snapshot
with sanitized merged `entries`, raw runtime `connections`, and inventory warnings.
Older daemons can report their local connection, but unmatched saved profiles
remain unavailable because shared-container inventory cannot be verified.

Use `vpn remove NAME_OR_ID` to delete a saved profile. Omitting its selector
opens a profile picker. Removal always requires interactive confirmation,
defaulting to No, and retains Tailscale identity volumes. There is no `--yes`
bypass. After confirmation it requires complete runtime inventory and a stopped
VPN; stop it and wait for container exit first. Remove never starts a daemon or
stops a VPN automatically.

The bundled skills and references are available without a daemon, connection,
or source checkout.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
