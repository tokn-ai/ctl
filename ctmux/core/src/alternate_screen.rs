//! Conservative alternate-screen hints, separate from terminal emulation.

mod state;

pub use state::AlternateScreenState;

use ctmux_proto::TuiHint;
use serde::{Deserialize, Serialize};

const MAX_CSI_PREFIX_BYTES: usize = 64;

/// Observes DEC alternate-screen controls for client presentation hints.
///
/// This deliberately is not an authoritative screen parser. The terminal
/// emulator owns buffer switching; this observer tracks independent mode bits
/// and preserves incomplete raw controls across private runtime checkpoints.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AlternateScreenTracker {
  parser: CsiObserver,
  active_modes: u8,
  tui_hint: TuiHint,
}

impl AlternateScreenTracker {
  /// The most recently established presentation hint.
  #[must_use]
  pub fn hint(&self) -> TuiHint {
    self.tui_hint
  }

  /// Clear the observer when the owning terminal performs a reset.
  pub fn reset(&mut self) -> Option<TuiHint> {
    let changed = self.tui_hint != TuiHint::Inline;
    *self = Self {
      tui_hint: TuiHint::Inline,
      ..Self::default()
    };
    changed.then_some(TuiHint::Inline)
  }

  /// Observe raw output without interpreting terminal contents.
  pub fn observe(&mut self, data: &[u8]) -> Option<TuiHint> {
    if data.is_empty() {
      return None;
    }

    let mut next_hint = if self.tui_hint == TuiHint::Unknown {
      TuiHint::Inline
    } else {
      self.tui_hint
    };
    for byte in data {
      if let Some((enable, modes)) = self.parser.feed(*byte) {
        if enable {
          self.active_modes |= modes;
        } else {
          self.active_modes &= !modes;
        }
        next_hint = if self.active_modes == 0 {
          TuiHint::Inline
        } else {
          TuiHint::AlternateScreen
        };
      }
    }

    if next_hint == self.tui_hint {
      None
    } else {
      self.tui_hint = next_hint;
      Some(next_hint)
    }
  }
}

/// A bounded raw observer for CSI private-mode set/reset sequences.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct CsiObserver {
  state: CsiObserverState,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CsiObserverState {
  #[default]
  Ground,
  Escape,
  Csi(Vec<u8>),
}

impl CsiObserver {
  fn feed(&mut self, byte: u8) -> Option<(bool, u8)> {
    match &mut self.state {
      CsiObserverState::Ground => {
        if byte == 0x1b {
          self.state = CsiObserverState::Escape;
        }
        None
      }
      CsiObserverState::Escape => {
        self.state = match byte {
          b'[' => CsiObserverState::Csi(Vec::new()),
          0x1b => CsiObserverState::Escape,
          _ => CsiObserverState::Ground,
        };
        None
      }
      CsiObserverState::Csi(bytes) => {
        if byte == 0x1b {
          self.state = CsiObserverState::Escape;
          return None;
        }

        bytes.push(byte);
        if (0x40..=0x7e).contains(&byte) {
          let sequence = std::mem::take(bytes);
          self.state = CsiObserverState::Ground;
          return alternate_screen_transition(&sequence);
        }

        // Keep the observer bounded even for arbitrary application output.
        if bytes.len() > MAX_CSI_PREFIX_BYTES {
          self.state = CsiObserverState::Ground;
        }
        None
      }
    }
  }
}

fn alternate_screen_transition(sequence: &[u8]) -> Option<(bool, u8)> {
  let (&final_byte, parameters) = sequence.split_last()?;
  if !matches!(final_byte, b'h' | b'l') || !parameters.starts_with(b"?") {
    return None;
  }

  let modes = parameters[1..]
    .split(|byte| *byte == b';')
    .filter_map(|parameter| std::str::from_utf8(parameter).ok())
    .filter_map(|parameter| parameter.parse::<u16>().ok())
    .fold(0_u8, |modes, parameter| {
      modes
        | match parameter {
          47 => 0b001,
          1047 => 0b010,
          1049 => 0b100,
          _ => 0,
        }
    });
  (modes != 0).then_some((final_byte == b'h', modes))
}
