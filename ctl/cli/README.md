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
`ctmuxd`, and `ctl-taskd`; remote VPNs also need `ctld`. Interactive Unix connections
offer to repair missing
or recognized older agents with a matching verified bundle, then retry once.
Repair checks any saved machine ID first, shows upload progress and speed, and
preserves running daemons. Ctrl-C cancels it; piped commands never prompt.
Bundles can come from
`CTL_REMOTE_BUNDLES_DIR`, local resources, `~/.tokn/ctl/agent-bundles`, the matching
official release, or an existing verified GitHub bundle artifact (requires `gh`).
Downloaded manifests and archives are cached at
`~/.tokn/ctl/agent-bundles/<revision>/<target>/` and verified again before reuse.
Schema-2 bundles include all four components and record their product/source identity and explicit
protocol support. Compatible local or cached bundles can be reused across CLI
releases and source revisions, including by development clients. Reuse checks
all service and companion contracts, the target, archive and binary checksums,
and agreement between the bundle-set and archived metadata. The exact current
source cache is checked first, followed by compatible entries in stable revision
order; this order does not imply which build is newest. Schema-1 bundles remain
eligible only for the exact clean client build. Downloads still require a clean,
identified CLI and match that source revision. Compatible cache entries work
offline and can serve any host with that platform.
Interrupted downloads never publish a partial entry; damaged cache entries are
downloaded again. Explicit bundle overrides still reject invalid contents.
Source developers can prepare their exact bundles with `pnpm agents:sync` from
`apps/desktop`. Windows remote companions must be installed manually. On macOS,
Touch ID-protected credential storage
requires the signed, provisioned `ctld` helper; Cargo installation alone does
not supply its Keychain entitlement. Setup selects the release matching this
CLI's version and architecture and requires that release to be published.
`CTLD_BIN` remains an explicit override. Otherwise a standalone macOS CLI prefers
a verified compatible shared `ctld.app`, then its own bundled helper, then a
nearby desktop bundle, sibling, or `PATH` executable. The desktop continues to
prefer its own bundled helper. Shared apps must match the native architecture and
advertise a shared ctld, lifecycle, and helper contract. Remote VPN routes require
ctld `1.1.13` and helper `1.1.2`. Their build identity is checked
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
Installation updates `selected/<target>-ctld1-lifecycle1-helper1`, which all
standalone CLI builds, including ordinary Cargo builds, can reuse after signature
and provisioning verification. Rebuild if the profile expires; existing daemons
require an explicit restart.

Use `ctl components update` for an explicit update. It defaults to this computer;
`--hosts work,jump-a` updates several saved hosts through their preferred routes,
and `--local` includes this computer in that batch. `--package ctl-agent` retains
the installed daemons; the default `--package full-bundle` installs all four
components from one compatible complete build. Updates preserve running services
and sessions; restart or reconnect separately.

```sh
ctl components update --hosts work,jump-a --package ctl-agent
ctl --host work --method vpn components update
ctl components update --hosts work --from /path/to/bundle-set --json
```

`--from` supplies a complete build directory or archive. Use `--local-build` for
four native binaries; local macOS full updates also require `--ctld-package`.
Otherwise updates use the pinned selection for each target. `components list`,
`sync`, and `select` manage the store and its selections. See the
[component update guide](https://github.com/tokn-ai/ctl/blob/main/docs/component-updates.md)
for source formats, cancellation, and per-host results.

VPN profiles remain in the local catalog. Use `ctl --host GATEWAY vpn start NAME_OR_ID`,
`vpn list`, and `vpn stop NAME_OR_ID` to run and manage a profile on an SSH host.
The gateway needs updated ctl-agent/ctld components and Docker or Podman. Its
VPN SOCKS5 listener stays on remote loopback; traffic travels through SSH.
Remote VPNs keep running until explicitly stopped, including after SSH disconnects.
Profile creation and removal remain local operations.

An ordered saved route can include `--gateway JUMP_ID --gateway vpn:PROFILE_ID`.
The VPN executes on the preceding SSH gateway. A first VPN step executes locally;
the existing `--vpn PROFILE_ID` option still selects a local VPN before the route.

## Use

```sh
ctl --help
ctl host create
ctl host list
ctl passwords
ctl passwords show ID
ctl passwords remove ID
ctl passwords clear
ctl vpn create
ctl vpn list
ctl vpn start NAME_OR_ID
ctl vpn stop NAME_OR_ID
ctl skill
ctl skill --list
```

`host create` opens a questionnaire and saves a host definition without
connecting or saving credentials. For scripts, use
`ctl host create NAME DESTINATION` with connection flags. With `--json`, prompts
use stderr and the saved host JSON is written to stdout. `host method add` adds an alternate
connection method to an existing host.

`passwords` (or `passwords list`) lists locally saved SSH passwords, legacy SSH
credentials, and identity-file passphrases through the selected signed helper.
`show` displays metadata only. Human-readable tables use compact `p-` and `k-`
IDs alongside names, accounts, targets or key paths, and state. Use the printed
ID, a full ID from JSON, a unique full-ID prefix, or an exact unique name with
`show` and `remove`. `--json` is available for all four actions and retains full
IDs. Listing and showing discover owned items directly from Keychain attributes
without reading secret values or starting a daemon. Keychain can request
authentication to complete discovery. Entries with missing or malformed metadata
remain visible with an `unknown` state; `show` explains what could not be checked.
Discovery completeness is separate from metadata quality. A failed scan reports
its specific reason, and an empty partial list does not claim no credentials are
saved. No metadata import command is needed.

`passwords remove` opens a picker when its selector is omitted and removes one
saved secret after interactive confirmation. `passwords clear` previews discovered
entries and, after confirmation, clears all owned SSH credentials and identity
passphrases, including legacy copies. All password commands require ctld helper
contract `1.1.4` for authoritative discovery; update the selected signed helper
if an older helper rejects it. Both removal actions require an interactive
terminal and have no `--yes` bypass. They retain private key files, hosts, VPN profiles, running
connections, and never-save preferences. These commands manage the local macOS
Keychain, so omit `--host`, `--method`, and `--remote-platform`.

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
