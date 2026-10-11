# ctmux-core

Session and terminal view model for ctmux.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

## Terminal observer checkpoints

The terminal's input-mode and alternate-screen observers have exact state
exports alongside the emulator checkpoint:

- `mouse::TerminalInputModes::export_state()` returns an opaque
  `TerminalInputModesState`. `TerminalInputModes::from_state(state)` restores
  mouse tracking and encoding bits, bracketed paste, modifyOtherKeys, and any
  unfinished control sequence.
- `alternate_screen::AlternateScreenTracker::export_state()` returns an opaque
  `AlternateScreenState`. `AlternateScreenTracker::from_state(state)` restores
  independent DEC 47/1047/1049 mode bits, the presentation hint, and any raw CSI
  prefix. Ctmuxd uses this shared observer.

The state types implement serde serialization/deserialization and expose
`version()`. Each representation is currently version 1, independent of the
public ctmux protocol. Callers choose the encoding and must bound encoded input
before deserialization. Import validates versions, known mode bits, bounded
parser fragments, and observer invariants, returning `ObserverStateError` for
invalid state. Restoration does not replay control sequences or discard a
pending fragment.

These exports cover the observers. Output journals, attachment leases, and
session registries still need separate state exports for full daemon hotreload.

Licensed under MIT.
