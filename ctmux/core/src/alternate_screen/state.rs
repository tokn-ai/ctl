use serde::{Deserialize, Serialize};

use super::{AlternateScreenTracker, CsiObserver, CsiObserverState, MAX_CSI_PREFIX_BYTES, TuiHint};
use crate::ObserverStateError;
use crate::observer_state::validate_version;

const ALTERNATE_SCREEN_STATE_VERSION: u32 = 1;

/// Private checkpoint of mode bits, presentation hint and raw CSI prefix.
///
/// Deserialize with a bounded encoded input, then call
/// [`AlternateScreenTracker::from_state`] to validate the representation. This
/// format version is independent of the public ctmux wire protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlternateScreenState {
  version: u32,
  parser: CsiObserverState,
  active_modes: u8,
  tui_hint: TuiHint,
}

impl AlternateScreenState {
  /// Version of this private checkpoint representation.
  #[must_use]
  pub fn version(&self) -> u32 {
    self.version
  }

  fn validate(&self) -> Result<(), ObserverStateError> {
    validate_version(
      "alternate screen",
      self.version,
      ALTERNATE_SCREEN_STATE_VERSION,
    )?;
    if self.active_modes & !0x07 != 0 {
      return Err(ObserverStateError::InvalidState(
        "unknown alternate-screen mode bits",
      ));
    }
    if (self.active_modes != 0) != (self.tui_hint == TuiHint::AlternateScreen) {
      return Err(ObserverStateError::InvalidState(
        "inconsistent alternate-screen hint",
      ));
    }
    if self.tui_hint == TuiHint::Unknown && self.parser != CsiObserverState::Ground {
      return Err(ObserverStateError::InvalidState(
        "unknown hint with consumed control bytes",
      ));
    }
    if let CsiObserverState::Csi(bytes) = &self.parser
      && (bytes.len() > MAX_CSI_PREFIX_BYTES
        || bytes
          .iter()
          .any(|byte| *byte == 0x1b || (0x40..=0x7e).contains(byte)))
    {
      return Err(ObserverStateError::InvalidState(
        "alternate-screen CSI prefix",
      ));
    }
    Ok(())
  }
}

impl AlternateScreenTracker {
  /// Capture the observer without interpreting or discarding pending bytes.
  #[must_use]
  pub fn export_state(&self) -> AlternateScreenState {
    AlternateScreenState {
      version: ALTERNATE_SCREEN_STATE_VERSION,
      parser: self.parser.state.clone(),
      active_modes: self.active_modes,
      tui_hint: self.tui_hint,
    }
  }

  /// Restore an exact observer after checking version and bounded parser state.
  ///
  /// # Errors
  ///
  /// Rejects unsupported versions, mode bits, inconsistent presentation hints
  /// and invalid or oversized prefixes before creating a mutable observer.
  pub fn from_state(state: AlternateScreenState) -> Result<Self, ObserverStateError> {
    state.validate()?;
    Ok(Self {
      parser: CsiObserver {
        state: state.parser,
      },
      active_modes: state.active_modes,
      tui_hint: state.tui_hint,
    })
  }
}

#[cfg(test)]
mod tests;
