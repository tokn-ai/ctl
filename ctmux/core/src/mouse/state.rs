use serde::{Deserialize, Serialize};

use super::{
  Control, MAX_CSI_PARAMETER_BYTES, ModifyOtherKeys, MouseModes, MouseTracking, SGR, SGR_PIXELS,
  TerminalInputModes, URXVT, UTF8,
};
use crate::ObserverStateError;
use crate::observer_state::validate_version;

const INPUT_MODES_STATE_VERSION: u32 = 1;

/// Exact application input modes and the unfinished observer control.
///
/// This is independent of the public ctmux wire protocol. Deserialization only
/// decodes the representation; [`TerminalInputModes::from_state`] checks its
/// version and invariants. Bound encoded input before deserializing external data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalInputModesState {
  version: u32,
  mouse_tracking: MouseTracking,
  mouse_encodings: u8,
  bracketed_paste: bool,
  modify_other_keys: ModifyOtherKeys,
  control: Control,
}

impl TerminalInputModesState {
  /// Version of this private checkpoint representation.
  #[must_use]
  pub fn version(&self) -> u32 {
    self.version
  }

  fn validate(&self) -> Result<(), ObserverStateError> {
    validate_version("input modes", self.version, INPUT_MODES_STATE_VERSION)?;
    if self.mouse_encodings & !(UTF8 | SGR | URXVT | SGR_PIXELS) != 0 {
      return Err(ObserverStateError::InvalidState(
        "unknown mouse encoding bits",
      ));
    }
    if let Control::Csi(parameters) = &self.control
      && (parameters.len() > MAX_CSI_PARAMETER_BYTES
        || !parameters.bytes().all(|byte| (b' '..=b'?').contains(&byte)))
    {
      return Err(ObserverStateError::InvalidState("input mode CSI prefix"));
    }
    Ok(())
  }
}

impl TerminalInputModes {
  /// Capture every mode and parser prefix without feeding restoration controls.
  #[must_use]
  pub fn export_state(&self) -> TerminalInputModesState {
    TerminalInputModesState {
      version: INPUT_MODES_STATE_VERSION,
      mouse_tracking: self.mouse.tracking,
      mouse_encodings: self.mouse.encodings,
      bracketed_paste: self.bracketed_paste,
      modify_other_keys: self.modify_other_keys,
      control: self.control.clone(),
    }
  }

  /// Restore a validated checkpoint without interpreting or discarding a prefix.
  ///
  /// # Errors
  ///
  /// Rejects unsupported versions, encoding bits and invalid or oversized CSI
  /// prefixes before creating a mutable observer.
  pub fn from_state(state: TerminalInputModesState) -> Result<Self, ObserverStateError> {
    state.validate()?;
    Ok(Self {
      mouse: MouseModes {
        tracking: state.mouse_tracking,
        encodings: state.mouse_encodings,
      },
      bracketed_paste: state.bracketed_paste,
      modify_other_keys: state.modify_other_keys,
      control: state.control,
    })
  }
}

#[cfg(test)]
mod tests;
