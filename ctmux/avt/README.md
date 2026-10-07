# ctmux-avt

ctmux's terminal emulator, derived from [asciinema/avt](https://github.com/asciinema/avt)
0.18.0 (upstream commit `567ba32d8cfe4fe4ff61967a0dbbb9c827bcb3a8`).
Upstream code remains under the Apache License 2.0; see [LICENSE](LICENSE).
Copyright © 2019 Marcin Kulik.

This copy adds an optional resize policy for interactive shells. With
`Vt::builder().reflow_cursor_line(false)`, column changes preserve the physical
rows of the cursor's logical line, and consume unused default blank rows below
it before moving output into scrollback. Shells such as zsh redraw this line
after SIGWINCH and rely on its physical cursor position. Other primary rows
continue to reflow. Alternate-screen resizing and the default resize policy
retain upstream behavior.

Erase commands also preserve the correct wrap links between physical rows.
Erasing a complete row severs its incoming wrap; erasing characters in the
middle of a row preserves its links. This lets completed output reflow
independently after a shell clears and redraws its editable line.

The policy is implemented in `buffer.rs`, `line.rs`, `terminal.rs`, and `vt.rs`.
ctmux daemon, cache, history projections, and TUI models select it explicitly.
The parser is retained across resizing, including pending escape sequences.

Keep upstream code and tests intact when updating this copy, then reapply and
verify the resize policy. The local package name keeps published ctmux archives
on this implementation rather than silently restoring the upstream dependency.
