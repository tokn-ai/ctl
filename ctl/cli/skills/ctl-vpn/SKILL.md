---
name: ctl-vpn
description: Start, inspect, and release ctl-managed OpenConnect or Tailscale containers and their local SOCKS5 endpoints, including pending Tailscale sign-in and shared ownership.
---

# ctl vpn

Use `ctl vpn` locally on a Unix client. `-H` is rejected; these commands manage
the local ctld's VPN interest, not a remote machine's VPN. Docker or Podman must
be running. Start launches the selected ctld if needed; status and stop do not.

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
Present the returned link to the user and inspect status after sign-in. Device
approval may also be required; read `message`. Pending authentication can remain
indefinitely and can be stopped. Report a usable connection only after
`state` is `connected`, `status_unavailable` is not true, and `endpoint` is
present. If reauthentication becomes necessary, the endpoint is withdrawn.

CLI starts do not add saved VPN profiles to the desktop app. For selecting an
existing saved VPN profile as an SSH host route, use
[ctl-host](../ctl-host/SKILL.md), available with `ctl skill ctl-host`.

## Inspect and use an endpoint

```sh
ctl vpn status --json
ctl vpn stop VPN_ID --json
```

Start and stop JSON return the affected connection object. Status returns a
snapshot with `connections`, `supports_multiple`, `supported_providers`,
`supports_tailscale_enrollment`, and optional `discovery_warnings`. Connection
fields include `vpn_id`, `provider`, `state`, `endpoint`, `running`,
`connection_id`, `container_name`, and optional `auth_url`, `message`,
`container_id`, `shared_container`, `locally_connected`, and
`status_unavailable`. States are `stopped`, `starting`, `connected`, and
`stopping`; providers are `openconnect` and `tailscale`.

Select by the returned `vpn_id`, not array position or container name. File-based
OpenConnect IDs derive from the canonical settings path and have a null
`connection_id`; Tailscale uses its supplied connection ID. Multiple connections
can coexist. An untargeted `ctl vpn stop` requires zero or one local connection;
discovered shared containers do not make that choice ambiguous.

Do not interpret an empty `connections` array with `discovery_warnings` as proof
that no VPN containers exist. A missing selected daemon returns unavailable
inventory without starting it. `status_unavailable: true` means retained
metadata could not be verified. Older owners may omit fields or report
`supports_multiple: false`; update that owner for multiple VPNs or targeted stop
rather than replacing a requested targeted stop with an untargeted one.

Use the connection's current `socks5h://127.0.0.1:PORT` endpoint. Ports are random
and can change after recreation; do not assume port 1080 or reuse a cached port.
Keep `socks5h` when configuring applications so destination names resolve inside
the container, including VPN DNS. Applications must explicitly use the proxy;
ctl does not install host VPN routes. OpenConnect's proxy supports TCP and does
not provide SOCKS5 UDP relay.

## Shared lifetime and release

ctld renews a separate heartbeat interest for each connection. The VPN can keep
running after the start CLI exits. `locally_connected: true` means this ctld
holds interest; `false` identifies a discovered container kept alive elsewhere.
The table's USE column shows `this ctld` or `shared`. Status inspection never
acquires or renews interest.

Stop releases only the selected daemon's interest and leaves ctld running. A
shared container may remain connected while another daemon holds interest; its
continued visibility is expected. After the last heartbeat expires, the
container removes itself, which may take up to 15 seconds. Verify the local
interest was released instead of forcing removal of a shared container.

Use the same daemon socket selection for start, status, and stop:
`CTLD_VPN_SOCKET_PATH` takes precedence over `CTLD_SOCKET_PATH`. Different ctld
endpoints can share compatible containers for the same user and engine.
Release every interested daemon before changing active routing settings.
Tailscale stop retains its durable identity volume and login; changing `--id`
creates a separate identity. Do not remove that volume for routine disconnects.
