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
Keep terminal/task companion versions aligned with the CLI. These daemons should
share the CLI's binary directory; `ctl setup` installs the macOS helper under
`~/.tokn/ctl/components/ctld/` and switches its `current` symlink after verifying
the full signed and notarized app bundle. It never restarts an existing daemon.

For remote sessions and tasks, the controlled machine needs `ctl-agent`,
`ctmuxd`, and `ctl-taskd`. Interactive Unix connections offer to repair missing
or recognized older agents with a matching verified bundle, then retry once.
Repair checks any saved machine ID first, shows upload progress and speed, and
preserves running daemons. Ctrl-C cancels it; piped commands never prompt.
Bundles must match the CLI's exact clean source revision. They can come from
`CTL_REMOTE_BUNDLES_DIR`, local resources, `~/.tokn/ctl/agent-bundles`, the matching
official release, or an existing verified GitHub bundle artifact (requires `gh`).
Source developers can prepare their exact bundles with `pnpm agents:sync` from
`apps/desktop`. Windows remote companions must be installed manually. On macOS, Touch ID-protected credential storage
requires the signed, provisioned `ctld` helper; Cargo installation alone does
not supply its Keychain entitlement. Setup selects the release matching this
CLI's version and architecture and requires that release to be published.
`CTLD_BIN` remains an explicit override. Otherwise a standalone macOS CLI prefers
a verified compatible shared `ctld.app`, then its own bundled helper, then a
nearby desktop bundle, sibling, or `PATH` executable. The desktop continues to
prefer its own bundled helper. Shared apps must match the native architecture and ctld protocol 12,
lifecycle protocol 1, and one-shot helper API 1. Their build identity is checked
against their own installation manifest; their release version, commit, and
fingerprint need not equal the CLI's.

Official macOS CLI downloads carry the complete matching signed `ctld.app`
inside the `ctl` executable. They install it under the same managed directory
when a command needs the helper and no compatible shared app is selected, or
when you run `ctl setup`, without downloading it. Running compatible daemons
and passive status reads are preserved. A dedicated
[build/release command](https://github.com/tokn-ai/ctl/blob/main/docs/ci-bundles.md#build-a-cli-with-ctld-embedded)
compiles and bundles the helper; ordinary Cargo installation does not embed it.

For local macOS development, use the shared Xcode provisioning flow from a checkout:

```sh
node scripts/dev/ctl-signed.mts --provision
# Choose your team in Xcode and build the provisioning target once.
node scripts/dev/ctl-signed.mts
target/ctl-dev/ctl --help
```

The build discovers and refreshes the provisioning profile, signs and embeds
`ctld.app`, and signs the CLI with the matching certificate. No notarization
credentials are needed. Development helpers use a separate cache under
`~/.tokn/ctl/components/ctld/development/` and leave release `current` unchanged.
Installation updates `selected/<target>-ctld12-lifecycle1-helper1`, which all
standalone CLI builds, including ordinary Cargo builds, can reuse after signature
and provisioning verification. Rebuild if the profile expires; existing daemons
require an explicit restart.

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
