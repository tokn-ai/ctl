# ctl-agent

SSH gateway for remote ctl terminal sessions, managed tasks, and VPNs.

Installs `ctl-agent`. Install it alongside `ctmuxd`, `ctl-taskd`, and `ctld` on a machine
that accepts the user's SSH connections. Once the packages are published:

```sh
cargo install --locked ctl-agent ctmuxd ctl-taskd ctld
```

Current full remote bundles also install `ctl` and `ctmux` for querying service
connections and terminal sessions from the controlled account. For a manual
installation of these commands, use `cargo install --locked ctl-cli ctmux-cli`.

The agent runs for the lifetime of a connection; the companion daemons preserve
sessions and tasks after disconnecting. Keep all four package versions aligned.
VPN containers and their heartbeat interests belong to remote `ctld` and survive
closing a VPN control or TCP channel. `ctl-agent vpn` exposes only bounded VPN
requests, never the complete broker socket. The remote bundle's macOS `ctld`
companion supports this VPN interface; the separately signed `ctld.app` continues
to provide macOS Keychain integration for ordinary local SSH authentication.

`ctl-agent listeners --protocol any` discovers TCP and UDP endpoints on Linux
and macOS. The argument-free command retains TCP-only output for older clients.
`ctl-agent forward-udp` serves the negotiated `ctl_forwarding 1.0.2` endpoint
used by ctld, with its destination sent through bounded stdin frames. Each
channel preserves datagram boundaries and multiplexes separate remote UDP peer
sockets within one SSH session. The earlier `1.0.1` single-peer contract remains
supported for existing clients.

`ctl-agent files` serves the negotiated `ctl_files 1.0.1` endpoint for bounded
metadata, file reads, directory pages, and Git status. The same endpoint runs on
independent Files streams under `ctl_transport 1.1.2`; earlier `1.0.1` transport
clients retain terminal, task, and UDP services. Each Files stream owns one
operation, and closing it cancels the work without affecting sibling requests.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
