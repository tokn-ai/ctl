---
name: ctl-host
description: Manage ctl's saved host catalog, connection methods and preferred routes, inspect passive SSH connection status, and connect or disconnect saved hosts. Use for ctl host operations; ordinary remote commands and file copies belong to ctl.
---

# ctl host

Use `ctl host` for saved host definitions shared with the desktop. A host represents one remote account/environment; its methods are alternate ways to reach that environment. These commands take a positional saved host name or ID, not `-H`/`--host`. Unsaved SSH aliases and raw destinations must first be saved to use host management.

## Inspect hosts and connection status

```sh
ctl host list --json
ctl host show work --json
ctl host status work --method VPN --json
```

List includes saved definitions only. Show includes settings, any previously learned remote identity/version, and per-method status. List and status JSON return arrays of `{host, statuses}` objects; show JSON returns one object. Select an ambiguous display name by stable ID. A method can also be selected by name or ID; without `--method`, show/status display all its methods.

Inspection observes existing ctld connections without starting ctld, authenticating SSH, or starting a VPN. Interpret status as follows:

| State | Meaning |
| --- | --- |
| `connected` | An existing SSH control endpoint responds. |
| `disconnected` | No connection is observed. |
| `paused` | The connection was explicitly disconnected. |
| `unknown` | Observation failed; inspect its `message`. |
| `unsupported` | Connection observation is unavailable on this platform. |

This is not a remote reachability check. Previously learned identity/version is saved metadata, not proof that the host is currently connected.

## Save and update definitions

```sh
ctl host create
ctl host create work 10.0.0.20 --user alice --json
ctl host create lab lab-ssh-alias --ssh-config --json
ctl host update work --name office --port 2222 --json
ctl host update office --clear port --json
```

Create saves one host with a first method named `SSH` by default; use `--method-name NAME` to choose another name. It does not connect or install remote components. `--ssh-config` marks the destination as an existing OpenSSH config alias; these operations do not edit SSH config.

Bare `ctl host create` opens a questionnaire in an interactive terminal. Pass both `NAME DESTINATION` and the required connection flags in scripts; omitted name or destination can be prompted for in a terminal. Prompts go to stderr, so `--json` leaves only the saved host JSON on stdout. Creation does not prompt for credentials. Root `host add` is no longer accepted; `host method add` still adds a method to an existing host.

Update preserves host/method IDs and any pinned remote identity. Omitted fields remain unchanged. It edits the preferred method unless `--method NAME_OR_ID` selects another. Use `--clear` for optional fields rather than supplying empty strings, and do not set and clear the same field in one operation. Consult `ctl host update --help` for available fields and clear values.

## Methods and routes

```sh
ctl host method add office VPN 10.0.0.20 --user alice --vpn company --json
ctl host method prefer office VPN --json
ctl host method update office VPN --destination 10.0.0.21 --json
ctl host update office --method VPN --clear vpn --json
```

Method management uses positional method names or IDs, not `--method`. Add accepts `--prefer` to select the new method immediately. The preferred method is used for subsequent connections unless a command selects another method. Every method must reach the same host environment; editing an address does not clear a pinned identity.

`--vpn ID` references a saved VPN profile running locally before the route.
`--gateway ID` references an existing saved gateway; repeat it in route order.
Use `--gateway vpn:PROFILE_ID` to place a VPN in that route. A first VPN executes
locally; a VPN immediately after an SSH step executes on that SSH host. Host
commands do not create gateways or VPN profiles. `--clear gateways` removes the
route. A method bound with `--tailscale-node-id ID` resolves that device's current
address; unavailable discovery fails rather than using a stale address.

## Connect, disconnect, and remove

```sh
ctl host connect office --method VPN
ctl host disconnect office --method VPN
ctl host disconnect office
```

Connect authenticates the preferred or selected method and prepares its VPNs in
route order. Remote VPN startup requires updated remote components and a container
engine on each execution host. It opens no shell and installs no remote component.
Disconnect pauses only the selected method, or all the host's saved methods when
omitted. Active channels may close; it does not terminate remote sessions or stop
the VPN itself. Connect/disconnect require a Unix client; catalog editing is also
available on Windows.

For requested deletion, use `ctl host method remove HOST METHOD --json` or `ctl host remove HOST --json`. A preferred method cannot be removed until another is preferred. Removing a host deletes only its definition; active connections, remote sessions, credentials, and workspace references remain.

## Catalog conflicts

The catalog is `~/.tokn/ctl/hosts.json`, or the path selected by `CTL_HOSTS_PATH`. Use the CLI's shared validation and atomic catalog updates instead of rewriting this file. On a concurrent-edit conflict, reread the current host and reapply only the intended changes; do not force an old snapshot over the newer catalog. Reload an already-open desktop to see CLI edits.
