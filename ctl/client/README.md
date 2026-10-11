# ctl-client

Client transports, SSH connections, and remote component management for ctl.

On Unix, `connection::ConnectionClient` provides typed connection status,
establishment, and disconnect actions. Frontends supply interactive prompts;
quiet actions never invoke them. The client reuses negotiated broker sockets.
`existing_master` passively returns an exact reusable control path before VPN
or gateway preparation. `recovery::RecoveryClient` holds a separate live-host
socket lease while a frontend is active and observes successful wake
authorization events. It quietly reconnects to an existing ctld with bounded
backoff, never starts a daemon, and closes its socket on stop or drop. The watch
requires ctld 1.1.16; older contracts keep their existing quiet behavior.
`component_status::observe_local` passively reports a chosen local owner without
starting it or discovering a replacement helper. CLI and desktop adapters keep
their own availability and presentation rules.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
