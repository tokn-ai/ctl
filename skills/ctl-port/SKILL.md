---
name: ctl-port
description: "Manage ctl's daemon-owned local SSH port forwards: add, inspect, update, and remove runtime listeners for a selected host connection method."
---

# ctl port

Use `ctl port` on a Unix client with an SSH destination selected through `-H`.
The listener runs on the client; `remote_host:remote_port` is reached from the
SSH server. For host or route selection, use [ctl-host](../ctl-host/SKILL.md),
available with `ctl skill ctl-host`.

## Manage a forward

```sh
ctl -H work port add 8080:127.0.0.1:80 --id work-web --json
ctl -H work port list --json
ctl -H work port remove work-web
```

`add` starts ctld and authenticates SSH as needed, then returns after the listener
becomes active. It can also start the saved VPN needed by that route. If it fails,
inspect `list --json` and the error; a failed add may leave a runtime definition
with an error or pending state.

Choose a stable, distinctive `--id` when later updates or cleanup are expected.
Without it, ctl generates a UUID and prints it. Repeating `add` with the same ID
reuses an identical active forward, or replaces its definition after cancelling
the previous listener. IDs belong to the selected ctld registry, so reusing an
ID for another host or route moves the existing forward rather than creating an
independent one.

List and remove select a connection method, and list does not open SSH. Preserve
the same host and `--method` for all operations on a nonpreferred route:

```sh
ctl -H work --method vpn port add 15432:127.0.0.1:5432 --id work-db
ctl -H work --method vpn port list --json
ctl -H work --method vpn port remove work-db
```

An empty list on another method does not prove the forward is gone.

## Specification and status

The specification is `[bind_address:]local_port:remote_host:remote_port`.
Both ports must be 1–65535. The default bind is `127.0.0.1`; only `127.0.0.1`
and `::1` are accepted. Public and wildcard binds are unavailable.
Bracket IPv6 addresses and quote the whole specification so shell globbing does
not consume the brackets:

```sh
ctl -H work port add '[::1]:8080:[2001:db8::2]:80' --id work-web-v6
```

Add and list `--json` output an array, including add's single result. Each item
contains `forward` with `forward_id`, `bind_address`, `local_port`, `remote_host`,
and `remote_port`, plus `state` and `message`. JSON states are `active`,
`waiting_for_authentication`, and `error`. Remove has no JSON option.
An active listener confirms forwarding setup; verify the destination service
separately when the requested outcome depends on it.

## Lifetime

Forwards outlive the invoking CLI and remain in ctld's runtime registry until
removed or that daemon exits. They are not saved across daemon restarts.
An SSH disconnect can leave the definition waiting for authentication; reconnect
the same method to activate it again. Remove by ID when the requested listener
is no longer needed, then list that same method to verify removal.
