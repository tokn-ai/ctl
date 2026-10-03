---
name: ctl-vpn
description: Create or remove saved VPN profiles, list their runtime state, and start or stop ctl-managed OpenConnect or Tailscale connections locally or over SSH, including pending Tailscale sign-in and shared ownership.
---

# ctl vpn

Use `ctl vpn` on a Unix client. Add `--host GATEWAY` to list, start, or stop VPNs
on an SSH host. Profiles and their secrets remain in the local catalog; remote
start sends the selected settings over the authenticated SSH channel after
checking the saved remote identity. Docker or Podman and updated ctl-agent/ctld
components must be available where the VPN executes. Local start launches ctld
if needed. Local list reads the saved catalog and passively probes the selected
daemon without acquiring or renewing heartbeat interest. Remote list authenticates
SSH but does not start the remote VPN service or VPNs. Remote start may prepare
the selected host's preceding VPN route; list and stop use its existing route.
Remote VPNs remain running after SSH disconnects until explicitly stopped.
Create and remove are local profile operations and reject `--host`.

Use `--gateway JUMP_ID --gateway vpn:PROFILE_ID` when saving an ordered host route.
A VPN after an SSH gateway executes on that gateway. A first VPN step, or the
legacy `--vpn PROFILE_ID` host option, executes locally. A VPN must be first or
immediately after an SSH step.

## Create a saved profile

```sh
ctl vpn create
```

Create opens an interactive questionnaire, asks for the provider and its settings,
then saves a profile in `~/.tokn/ctl/vpns.json`, shared with the desktop VPN page.
`CTL_VPNS_PATH` selects another catalog. OpenConnect asks for a gateway, username,
masked password, and optional authentication method and connectivity-check target.
Tailscale asks for an optional device hostname and whether to accept advertised
subnet routes. The questionnaire validates answers before saving and assigns a
stable profile ID. Ctrl-C or Esc
cancels without changing the catalog. Creation does not contact ctld, build an
image, or start a container. It requires an interactive terminal; never supply
passwords in command arguments, diagnostic output, or a shell script.
`create --json` keeps prompts on stderr and returns only the saved profile's
sanitized metadata on stdout; it still requires an interactive terminal.

For OpenConnect setup or a missing/incompatible image, read
[setup.md](references/setup.md) with
`ctl skill ctl-vpn --file references/setup.md`.

## List and start a saved profile

```sh
ctl vpn list --json
ctl vpn start NAME_OR_ID --json
```

Saved profiles are shared with the desktop VPN page in
`~/.tokn/ctl/vpns.json`. `CTL_VPNS_PATH` selects another catalog. List includes
profiles whose containers have been removed, alongside runtime connections and
compatible shared containers. An unmatched saved profile is disconnected only
when runtime inventory is complete; otherwise it is unavailable. Runtime-only
connections remain visible. The table shows NAME, PROVIDER, STATE, SERVER/TAILNET,
USERNAME, SOCKS5 ENDPOINT, and VPN ID. The USE column appears only when at least
one displayed VPN is marked `shared`. Saved display metadata never includes
passwords or URL credentials, paths, queries, or fragments.

Start selects an exact stable `connection_id` first, then a unique exact name.
Use the ID from list if multiple profiles have the same name. Start reads the
profile's credentials privately, reuses a compatible container, or recreates a
missing container. Run it again to recover after container removal or daemon
restart. It returns the affected connection object and does not add a profile to
the saved catalog. Read the current endpoint from that result or list because
the port may change on recreation.

Omitting the selector opens a saved-profile picker in an interactive terminal.
Scripts must supply a selector. OpenConnect uses Array Networks authentication
and waits for the VPN and proxy to become ready. It requires the compatible image
described in setup. Tailscale downloads its pinned official image on first use
and retains the same device login when restarting the same profile. Its
authentication is separate from an installed Tailscale client on the host.

A successful start may return `state: "starting"` and `auth_url` for sign-in.
Present the returned link to the user and run list after sign-in. Device
approval may also be required; read `message`. Pending authentication can remain
indefinitely and can be stopped. Report a usable connection only after
`state` is `connected`, `status_unavailable` is not true, and `endpoint` is
present. If reauthentication becomes necessary, the endpoint is withdrawn.

For selecting an existing saved VPN profile as an SSH host route, use
[ctl-host](../ctl-host/SKILL.md), available with `ctl skill ctl-host`.

## Inspect and use an endpoint

```sh
ctl vpn list --json
ctl vpn stop NAME_OR_ID --json
```

Start and stop JSON return the affected connection object. List returns
a snapshot with sanitized merged `entries`, raw runtime `connections`,
`supports_multiple`, `supported_providers`, `supports_tailscale_enrollment`, and
optional `discovery_warnings`. If the saved catalog cannot be read, list retains
the runtime entries and reports `profile_warnings`. Runtime connection
fields include `vpn_id`, `provider`, `state`, `endpoint`, `running`,
`connection_id`, `container_name`, and optional `auth_url`, `message`,
`container_id`, `shared_container`, `locally_connected`, and
`status_unavailable`. States are `stopped`, `starting`, `connected`, and
`stopping`; providers are `openconnect` and `tailscale`.

Stop accepts an exact saved profile ID, a unique exact profile name, or a runtime
`vpn_id` from list. Use a runtime ID for a connection without a saved profile;
do not select by array position or container name. Multiple connections can
coexist. Omitting the stop selector opens a picker of local connections in an
interactive terminal when the daemon reports `supports_multiple: true`. A legacy
daemon reports only one local connection; with an omitted selector, the CLI uses
its untargeted stop directly instead of a picker. In scripts, an untargeted stop
requires zero or one local connection; discovered
shared containers do not make that choice ambiguous.

An empty runtime `connections` array can coexist with saved profiles in `entries`.
Do not interpret an empty `connections` array with `discovery_warnings` as proof
that no VPN containers exist. A missing selected daemon leaves unmatched saved
profiles unavailable without starting it. `status_unavailable: true` means retained
metadata could not be verified. Older owners may omit fields or report
`supports_multiple: false`; update that owner for multiple VPNs or targeted stop
rather than replacing a requested targeted stop with an untargeted one.
Unmatched saved profiles remain unavailable with these local-only owners.

Use the connection's current `socks5h://127.0.0.1:PORT` endpoint. Ports are random
and can change after recreation; do not assume port 1080 or reuse a cached port.
For a remote VPN, this loopback endpoint belongs to the remote SSH host. An
ordered ctl route reaches it through SSH; the port is not exposed locally.
Keep `socks5h` when configuring applications so destination names resolve inside
the container, including VPN DNS. Applications must explicitly use the proxy;
ctl does not install host VPN routes. OpenConnect's proxy supports TCP and does
not provide SOCKS5 UDP relay.

## Shared lifetime and release

ctld renews a separate heartbeat interest for each connection. The VPN can keep
running after the start CLI exits. `locally_connected: true` means the selected
daemon holds heartbeat interest, displayed as `owned`; `false` identifies a
discovered container kept alive elsewhere, displayed as `shared`. USE appears
only when a displayed VPN has `locally_connected: false`; the JSON fields are
unchanged. List inspection never acquires or renews interest.

Stop releases only the selected daemon's interest and leaves ctld running. A
shared container may remain connected while another daemon holds interest; its
continued visibility is expected. After the last heartbeat expires, the
container removes itself, which may take up to 15 seconds. Verify the local
interest was released instead of forcing removal of a shared container.

Use the same daemon socket selection for start, list, and stop:
`CTLD_VPN_SOCKET_PATH` takes precedence over `CTLD_SOCKET_PATH`. Different ctld
endpoints can share compatible containers for the same user and engine.
Release every interested daemon before changing active routing settings.
Tailscale stop retains its durable identity volume and login; creating a new
profile creates a separate identity. Do not remove that volume for routine
disconnects.

## Remove a saved profile

```sh
ctl vpn remove NAME_OR_ID
```

Remove selects an exact saved profile ID before a unique exact name. Omitting
the selector opens a saved-profile picker, like start. It always asks for
interactive confirmation, defaulting to No; `--json` does not bypass that prompt
and there is no `--yes` flag. Choosing No or cancelling leaves the catalog
unchanged without querying ctld.

After confirmation, remove passively checks runtime inventory. The selected VPN
must be stopped and inventory must be complete; active containers (including
shared ones), stopping, or unverified state block deletion. Release every interested daemon's heartbeat and
wait for container exit first. Remove never starts a daemon or stops a VPN
automatically. Missing, incomplete, or legacy inventory must be resolved by
starting or updating ctld before retrying.

Removal deletes only the saved catalog entry. Its JSON result contains exactly
`removed: true`, `connection_id`, `name`, and `provider`. Tailscale identity
volumes are retained; removal does not revoke a device in the remote tailnet.
Use stop for a routine disconnect when the saved profile should remain available.
