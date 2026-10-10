# ctl-client

Client transports, SSH connections, and remote component management for ctl.

On Unix, `connection::ConnectionClient` provides typed connection status,
establishment, and disconnect actions. Frontends supply interactive prompts;
quiet actions never invoke them. The client reuses negotiated broker sockets.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
