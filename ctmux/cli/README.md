# ctmux-cli

Installs the `ctmux` command for persistent terminal sessions and shared views.
It includes the interactive terminal UI.

Once the packages are published, install the client and its companion daemon:

```sh
cargo install --locked ctmux-cli ctmuxd
ctmux new -s work
ctmux --resume
ctmux ls
ctmux attach -t work
```

`ctmux --resume` opens the existing-session picker, or creates one shell when
no sessions exist. `--socket PATH` and `--prefix Ctrl+a` still apply; resume
cannot be combined with another subcommand. Over SSH, use
`ctl -H HOST ctmux --resume`.

Inside the TUI, Ctrl+B then s opens the session manager. Enter attaches, `n`
creates, `r` renames, and `x` confirms whole-session termination. Leave a new
name blank and press Enter for an automatic name such as `session-1`. Ctrl+B
then d detaches while work keeps running. Rename requires ctmuxd 1.1.20.

Mouse selection keeps its position by default; press `y`, Enter, Alt+W, or
Ctrl+W to copy. Configure copying on release and what follows a copy in
`~/.tokn/ctl/ctmux.json`; see [selection settings](../../apps/tui/README.md#selection-settings).

Keep `ctmux` and `ctmuxd` in the same binary directory, or set `CTMUXD_BIN` to
the daemon executable. Cargo does not install a dependency's executables.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
