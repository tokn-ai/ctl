# ctmux-cli

Installs the `ctmux` command for persistent terminal sessions and shared views.
It includes the interactive terminal UI.

Once the packages are published, install the client and its companion daemon:

```sh
cargo install --locked ctmux-cli ctmuxd
ctmux new -s work
ctmux ls
ctmux attach -t work
```

Keep `ctmux` and `ctmuxd` in the same binary directory, or set `CTMUXD_BIN` to
the daemon executable. Cargo does not install a dependency's executables.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
