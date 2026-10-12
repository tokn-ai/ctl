//! Complete emulator state, independent of screen rendering and runtime handles.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::parser::ParserState;
use crate::terminal::state::TerminalState;

const VT_STATE_VERSION: u32 = 1;

/// Opaque, versioned checkpoint of a terminal emulator.
///
/// Serialize this value with a caller-selected serde format. Deserialization
/// only decodes the representation; [`crate::Vt::from_state`] validates its
/// version, shape and resource limits before constructing an emulator. Bound
/// the encoded input size before deserialization when reading external data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VtState {
    version: u32,
    parser: ParserState,
    terminal: TerminalState,
}

impl VtState {
    /// Format version of this checkpoint, independent of the ctmux wire protocol.
    pub fn version(&self) -> u32 {
        self.version
    }

    pub(crate) fn new(parser: ParserState, terminal: TerminalState) -> Self {
        Self {
            version: VT_STATE_VERSION,
            parser,
            terminal,
        }
    }

    pub(crate) fn validate(&self, limits: &StateLimits) -> Result<(), StateError> {
        validate_version("vt", self.version, VT_STATE_VERSION)?;
        self.parser.validate()?;
        self.terminal.validate(limits)
    }

    pub(crate) fn into_parts(self) -> (ParserState, TerminalState) {
        (self.parser, self.terminal)
    }
}

/// Bounds checked before imported state is turned into mutable terminal data.
///
/// Line and cell bounds apply to both buffers combined, including scrollback.
/// These do not replace an encoded-input byte limit at the deserialization layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateLimits {
    pub max_cols: usize,
    pub max_rows: usize,
    pub max_lines: usize,
    pub max_cells: usize,
    pub max_scrollback_limit: usize,
}

impl Default for StateLimits {
    fn default() -> Self {
        Self {
            max_cols: 4096,
            max_rows: 4096,
            max_lines: 262_144,
            max_cells: 16_777_216,
            max_scrollback_limit: 262_144,
        }
    }
}

/// A checkpoint cannot be restored without violating the emulator's invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    UnsupportedVersion {
        component: &'static str,
        version: u32,
    },
    InvalidState(&'static str),
    LimitExceeded(&'static str),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { component, version } => {
                write!(f, "unsupported {component} state version {version}")
            }
            Self::InvalidState(reason) => write!(f, "invalid terminal state: {reason}"),
            Self::LimitExceeded(limit) => write!(f, "terminal state exceeds {limit}"),
        }
    }
}

impl std::error::Error for StateError {}

pub(crate) fn validate_version(
    component: &'static str,
    version: u32,
    expected: u32,
) -> Result<(), StateError> {
    if version == expected {
        Ok(())
    } else {
        Err(StateError::UnsupportedVersion { component, version })
    }
}

pub(crate) fn validate_pen(pen: &crate::Pen) -> Result<(), StateError> {
    if pen.attrs & !0x1f == 0 {
        Ok(())
    } else {
        Err(StateError::InvalidState("unknown pen attributes"))
    }
}

pub(crate) fn validate_size(
    cols: usize,
    rows: usize,
    limits: &StateLimits,
) -> Result<(), StateError> {
    if cols == 0 || rows == 0 {
        return Err(StateError::InvalidState("zero buffer dimension"));
    }
    if cols > limits.max_cols || rows > limits.max_rows {
        return Err(StateError::LimitExceeded("buffer dimensions"));
    }
    if cols > isize::MAX as usize || rows > isize::MAX as usize {
        return Err(StateError::InvalidState(
            "buffer dimension exceeds coordinate range",
        ));
    }
    Ok(())
}

pub(crate) fn validate_scrollback_limit(
    limit: Option<usize>,
    limits: &StateLimits,
) -> Result<(), StateError> {
    if limit.is_some_and(|limit| limit > limits.max_scrollback_limit) {
        Err(StateError::LimitExceeded("scrollback limit"))
    } else {
        Ok(())
    }
}
