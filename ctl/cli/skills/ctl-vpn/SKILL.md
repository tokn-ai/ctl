---
name: ctl-vpn
description: List and connect saved VPN profiles, or start, inspect, and release ctl-managed OpenConnect or Tailscale containers and their local SOCKS5 endpoints, including pending Tailscale sign-in and shared ownership.
---

# ctl vpn

Use `ctl vpn` locally on a Unix client. `-H` is rejected; these commands manage
the local ctld's VPN interest, not a remote machine's VPN. Connecting or starting
a VPN requires Docker or Podman to be running. Connect and start launch the
selected ctld if needed; list and stop do not. List reads the saved catalog and
passively probes the selected daemon without acquiring or renewing heartbeat
interest.

## Connect a saved profile

```sh
ctl vpn list --json
ctl vpn connect NAME_OR_ID --json
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

Connect selects an exact stable `connection_id` first, then a unique exact name.
Use the ID from list if multiple profiles have the same name. Connect reads the
profile's credentials privately, reuses a compatible container, or recreates a
missing container. Run it again to recover after container removal or daemon
restart. It returns the affected connection object and does not add a profile to
the saved catalog. Read the current endpoint from that result or list because
the port may change on recreation.

OpenConnect still requires the compatible image described in
[setup.md](references/setup.md). A saved Tailscale profile may require browser
sign-in; follow the returned `auth_url` and readiness guidance below.

## Start the chosen provider

For OpenConnect, read [setup.md](references/setup.md) with
`ctl skill ctl-vpn --file references/setup.md` when preparing the private
settings file or resolving a missing/incompatible image. The image must already
support ctl's shared heartbeat protocol.

```sh
ctl vpn start --env-file /absolute/path/to/company.env --json
```

`--env-file` defaults to `.env` relative to the invoking directory. This is an
Array Networks OpenConnect connection. Start waits for the VPN and proxy to
become ready. Repeating a start for the same settings path reuses its active
connection; editing the file does not reconfigure that running connection.

For Tailscale, supply a stable ID and reuse it to keep the device's login:

```sh
ctl vpn start-tailscale --id my-tailnet --hostname ctl-work --json
```

`--name` sets a display name and defaults to `Tailscale`. Add `--accept-routes`
only when advertised subnet routes are wanted. The official pinned image is
downloaded on first use; no OpenConnect image build is needed. This container's
authentication is separate from an installed Tailscale client on the host.

A successful start may return `state: "starting"` and `auth_url` for sign-in.
Present the returned link to the user and run list after sign-in. Device
approval may also be required; read `message`. Pending authentication can remain
indefinitely and can be stopped. Report a usable connection only after
`state` is `connected`, `status_unavailable` is not true, and `endpoint` is
present. If reauthentication becomes necessary, the endpoint is withdrawn.

CLI starts do not add saved VPN profiles to the desktop app. For selecting an
existing saved VPN profile as an SSH host route, use
[ctl-host](../ctl-host/SKILL.md), available with `ctl skill ctl-host`.

## Inspect and use an endpoint

```sh
ctl vpn list --json
ctl vpn stop VPN_ID --json
```

Connect, start, and stop JSON return the affected connection object. List returns
a snapshot with sanitized merged `entries`, raw runtime `connections`,
`supports_multiple`, `supported_providers`, `supports_tailscale_enrollment`, and
optional `discovery_warnings`. If the saved catalog cannot be read, list retains
the runtime entries and reports `profile_warnings`. Runtime connection
fields include `vpn_id`, `provider`, `state`, `endpoint`, `running`,
`connection_id`, `container_name`, and optional `auth_url`, `message`,
`container_id`, `shared_container`, `locally_connected`, and
`status_unavailable`. States are `stopped`, `starting`, `connected`, and
`stopping`; providers are `openconnect` and `tailscale`.

Select by the returned `vpn_id`, not array position or container name. File-based
OpenConnect IDs derive from the canonical settings path and have a null
`connection_id`; saved profiles and Tailscale use their stable connection IDs.
Multiple connections can coexist. An untargeted `ctl vpn stop` requires zero or
one local connection; discovered shared containers do not make that choice
ambiguous.

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

Use the same daemon socket selection for connect, start, list, and stop:
`CTLD_VPN_SOCKET_PATH` takes precedence over `CTLD_SOCKET_PATH`. Different ctld
endpoints can share compatible containers for the same user and engine.
Release every interested daemon before changing active routing settings.
Tailscale stop retains its durable identity volume and login; changing `--id`
creates a separate identity. Do not remove that volume for routine disconnects.
