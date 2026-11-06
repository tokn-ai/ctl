# ctl-agent

SSH gateway for remote ctl terminal sessions, managed tasks, and VPNs.

Installs `ctl-agent`. Install it alongside `ctmuxd`, `ctl-taskd`, and `ctld` on a machine
that accepts the user's SSH connections. Once the packages are published:

```sh
cargo install --locked ctl-agent ctmuxd ctl-taskd ctld
```

The agent runs for the lifetime of a connection; the companion daemons preserve
sessions and tasks after disconnecting. Keep all four package versions aligned.
VPN containers and their heartbeat interests belong to remote `ctld` and survive
closing a VPN control or TCP channel. `ctl-agent vpn` exposes only bounded VPN
requests, never the complete broker socket. The remote bundle's macOS `ctld`
companion supports this VPN interface; the separately signed `ctld.app` continues
to provide macOS Keychain integration for ordinary local SSH authentication.

`ctl-agent listeners --protocol any` discovers TCP and UDP endpoints on Linux
and macOS. The argument-free command retains TCP-only output for older clients.
`ctl-agent forward-udp` serves the negotiated `ctl_forwarding 1.0.1` endpoint
used by ctld, with its destination sent through bounded stdin frames. Each
channel preserves datagram boundaries and owns one remote UDP peer socket.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
