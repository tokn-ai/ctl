# Proposal 0008: Native connection commands and OpenSSH compatibility

- Status: Implemented
- Created: 2026-10-01

This extends the command scope of [Proposal 0002](0002-ctl.md) while retaining
its fixed rmux/task service protocols and remote authentication boundary.

## Motivation

Terminal users need the same saved hosts, selected routes, authentication, and
connection reuse as the desktop. In particular, an ordinary SSH invocation
cannot discover a private ctld SOCKS/VPN master from its destination alone.

## Command surface

- `ctl [-H host] shell` creates and attaches a persistent rmux session.
  `--session name` attaches or creates that name; `--plain` opens an ordinary shell.
- `ctl [-H host] exec -- program arguments...` runs once without a PTY and
  preserves the exit status and input/output streams. Unix remote arguments
  are quoted individually; shell expressions require an explicit shell command.
- `ctl -H host port add/list/remove` manages local loopback listeners through
  the existing ctld registry. Runtime forwards outlive the client, are visible
  through desktop Ports refresh, and are not implicitly persisted by the desktop.
- `ctl ssh host [command...]` and `ctl scp source destination` retain OpenSSH's
  ordinary shell, execution, and copy semantics. They do not create rmux sessions.

- `ctl host list/show/status` inspect saved hosts and passive per-method ctld
  state. `add/update/remove` edit the shared catalog; `method` manages alternate
  routes and the preferred method. Explicit `connect/disconnect` control SSH
  connections without opening a shell. Host removal only removes its definition.

## Shared resolution and transport

A host selects a saved catalog name or stable ID first. Unknown names fall back
to ordinary SSH-config and DNS resolution. Ambiguous display names fail; stable
IDs remain usable. `--method`, placed before compatibility subcommands, chooses
a named method or ID instead of the preferred method. Explicit accounts override
saved accounts. Saved Tailscale device references resolve against current device
discovery rather than reusing an old address.

Host and route types, validation, and Tailscale discovery are shared Rust code.
The desktop keeps its existing catalog schema and location. The CLI reads that
catalog and starts a selected saved VPN when needed. It never writes SSH config.

Compatible Unix saved-host operations ask ctld for an authenticated master and
pass its exact control socket to OpenSSH. A vanished selected master cannot
silently fall back to a direct connection. User-supplied connection options use
OpenSSH directly with saved defaults, respecting explicit keys, configuration,
proxies, and control sockets rather than ignoring them on a reused connection.
Unknown raw destinations retain ordinary OpenSSH behavior.

SCP launches ctl as its SSH transport helper. Each local SSH subprocess resolves
its own destination, so the ordinary copy parser, progress display, protocol,
and remote-to-remote behavior remain owned by OpenSSH. An explicit `scp -S`
keeps the user's custom transport. Prompt input comes from the controlling
terminal, never the stdin stream carrying a command or file-transfer protocol.

## Invariants

1. Ordinary SSH operations require no remote ctl component or new listener.
2. Persistent rmux/task operations retain the fixed agent service transport;
   pinned saved identities are verified before those service requests.
3. A route never reuses a different route's private master merely because the
   destination IP matches.
4. Explicit OpenSSH options take precedence over saved defaults.
5. CLI-created runtime forwards are not turned into workspace restore preferences.
6. Neither CLI inspection nor desktop forward discovery authenticates a host.

## Detailed specifications

- [CLI usage](../../README.md#shells-commands-and-file-copies)
- [Service transport](../ctl-protocol.md)
- [Architecture](../architecture.md#remote-control-boundary)
