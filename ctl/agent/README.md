# ctl-agent

SSH gateway for remote ctl terminal sessions and managed tasks.

Installs `ctl-agent`. Install it alongside `ctmuxd` and `ctl-taskd` on a machine
that accepts the user's SSH connections. Once the packages are published:

```sh
cargo install --locked ctl-agent ctmuxd ctl-taskd
```

The agent runs for the lifetime of a connection; the companion daemons preserve
sessions and tasks after disconnecting. Keep all three package versions aligned.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
