# ctmux-client

Client library for persistent ctmux terminal sessions and views.

`session::SessionClient` provides typed list, create, terminate, and attach
actions on a selected transport. `AttachmentControl` queues lease and detach
requests; authoritative outcomes arrive through the controller's event stream.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
