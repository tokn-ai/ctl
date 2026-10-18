# ctl-ipc

Local connection broker protocol and lifecycle management for ctld.

Unix uses an owner-only Unix socket; Windows uses an owner-restricted local
named pipe. Both transports share framing, protocol negotiation, lifecycle
inspection, and coordinated daemon startup. `CTLD_RUNTIME_DIR` selects an
isolated endpoint on either platform; Windows hashes that directory into its
pipe name.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
