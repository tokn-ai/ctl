//! Synthetic byte decoding and live terminal state for the isolated exec proof.
//! Production daemon startup and its output decoder do not use this module.

use avt::{Vt, VtState};
use ctmux_core::alternate_screen::{AlternateScreenState, AlternateScreenTracker};
use ctmux_core::mouse::{TerminalInputModes, TerminalInputModesState};
use serde::{Deserialize, Serialize};

use super::ProofResult;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct TerminalState {
  pub vt_state: VtState,
  pub input_modes_state: TerminalInputModesState,
  pub alternate_screen_state: AlternateScreenState,
  pub utf8_pending: Vec<u8>,
}

pub(super) struct TerminalRuntime {
  vt: Vt,
  input_modes: TerminalInputModes,
  alternate_screen: AlternateScreenTracker,
  utf8_pending: Vec<u8>,
}

impl TerminalRuntime {
  pub fn new() -> Self {
    Self {
      vt: Vt::builder().size(12, 4).scrollback_limit(64).build(),
      input_modes: TerminalInputModes::default(),
      alternate_screen: AlternateScreenTracker::default(),
      utf8_pending: Vec::new(),
    }
  }

  pub fn from_state(state: TerminalState) -> ProofResult<Self> {
    if !state.utf8_pending.is_empty()
      && (state.utf8_pending.len() > 3
        || !std::str::from_utf8(&state.utf8_pending)
          .is_err_and(|error| error.error_len().is_none()))
    {
      return Err("checkpoint contains an invalid incomplete UTF-8 prefix".into());
    }
    Ok(Self {
      vt: Vt::from_state(state.vt_state)?,
      input_modes: TerminalInputModes::from_state(state.input_modes_state)?,
      alternate_screen: AlternateScreenTracker::from_state(state.alternate_screen_state)?,
      utf8_pending: state.utf8_pending,
    })
  }

  pub fn export_state(&self) -> TerminalState {
    TerminalState {
      vt_state: self.vt.export_state(),
      input_modes_state: self.input_modes.export_state(),
      alternate_screen_state: self.alternate_screen.export_state(),
      utf8_pending: self.utf8_pending.clone(),
    }
  }

  pub fn feed_byte(&mut self, byte: u8) -> ProofResult {
    self.alternate_screen.observe(&[byte]);
    self.utf8_pending.push(byte);
    let text = match std::str::from_utf8(&self.utf8_pending) {
      Ok(text) => text,
      Err(error) if error.error_len().is_none() && self.utf8_pending.len() <= 3 => return Ok(()),
      Err(error) => return Err(error.into()),
    };
    for ch in text.chars() {
      self.vt.feed(ch);
      self.input_modes.feed(ch);
    }
    self.utf8_pending.clear();
    Ok(())
  }
}
