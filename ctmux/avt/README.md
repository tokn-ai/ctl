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

## Runtime checkpoints

`Vt::export_state()` returns an opaque `VtState`; `Vt::from_state(state)`
restores it after validating its version, shape, and resource bounds. This
captures both screen buffers and scrollback, cell attributes and wrap links,
cursor and saved contexts, modes, margins, tabs, charsets, resize policy, pending
scrollback cleanup, dirty lines, and unfinished parser controls. Export does not
drain changes or collect history. `Vt::dump()` remains a rendering operation and
does not provide this complete runtime checkpoint.

For standalone parser users, `Parser::export_state()` and `Parser::from_state()`
use the opaque `avt::parser::ParserState`, including unfinished control parameters.
Both state types implement serde serialization/deserialization and expose
`version()`. Their current representation version is 1, independent of product
and ctmux wire protocol versions. Callers choose the encoding; JSON is used in
tests, rather than required by the library.

`Vt::from_state_with_limits(state, limits)` accepts custom `StateLimits` for
columns, rows, aggregate lines and cells across both buffers, and configured
scrollback capacity. The default limits are 4,096 columns and rows, 262,144
aggregate lines, 16,777,216 aggregate cells, and 262,144 scrollback capacity.
Unsupported versions, invalid state, and exceeded limits return `StateError`
before a mutable emulator is constructed. Deserialization can allocate before
that validation: callers reading external checkpoints must also enforce an
encoded-byte limit before decoding.

The parser consumes Unicode characters. Partial UTF-8 bytes belong to the
caller's decoder and must be preserved separately. The [continuation tests](tests/runtime_state.rs)
compare uninterrupted processing with prefix → serialized checkpoint → restored
suffix processing, including control fragments, inactive buffers, resize/reflow,
and deferred scrollback cleanup. These APIs preserve emulator state; native PTYs,
daemon registries, attachment leases, and output journals need their own exports.

Keep upstream code and tests intact when updating this copy, then reapply and
verify the resize policy and runtime checkpoint APIs. The local package name
keeps published ctmux archives on this implementation rather than silently
restoring the upstream dependency.
