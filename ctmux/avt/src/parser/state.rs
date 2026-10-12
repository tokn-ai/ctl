use serde::{Deserialize, Serialize};

use super::{Param, Parser, State, MAX_PARAM_LEN, PARAMS_LEN};
use crate::state::{validate_version, StateError};

const PARSER_STATE_VERSION: u32 = 1;

/// Exact parser checkpoint, including an unfinished control sequence.
///
/// This parser consumes Unicode characters. An incomplete UTF-8 byte sequence
/// belongs to the caller's decoder and must be checkpointed separately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParserState {
    version: u32,
    state: State,
    params: [ParamState; PARAMS_LEN],
    cur_param: usize,
    intermediate: Option<char>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParamState {
    cur_part: usize,
    parts: [u16; MAX_PARAM_LEN],
}

impl ParserState {
    /// Format version of this parser checkpoint.
    pub fn version(&self) -> u32 {
        self.version
    }

    pub(crate) fn validate(&self) -> Result<(), StateError> {
        validate_version("parser", self.version, PARSER_STATE_VERSION)?;
        if self.cur_param >= PARAMS_LEN {
            return Err(StateError::InvalidState("parser parameter index"));
        }
        if self
            .params
            .iter()
            .any(|param| param.cur_part >= MAX_PARAM_LEN)
        {
            return Err(StateError::InvalidState("parser subparameter index"));
        }
        if self.intermediate.is_some_and(|ch| {
            !('\u{20}'..='\u{2f}').contains(&ch) && !('\u{3c}'..='\u{3f}').contains(&ch)
        }) {
            return Err(StateError::InvalidState("parser intermediate character"));
        }
        Ok(())
    }
}

impl Parser {
    /// Capture parser state without dispatching or reconstructing escape sequences.
    pub fn export_state(&self) -> ParserState {
        ParserState {
            version: PARSER_STATE_VERSION,
            state: self.state,
            params: std::array::from_fn(|index| ParamState {
                cur_part: self.params[index].cur_part,
                parts: self.params[index].parts,
            }),
            cur_param: self.cur_param,
            intermediate: self.intermediate,
        }
    }

    /// Restore an exact checkpoint after validating its version and array indexes.
    pub fn from_state(state: ParserState) -> Result<Self, StateError> {
        state.validate()?;
        Ok(Self {
            state: state.state,
            params: state.params.map(|param| Param {
                cur_part: param.cur_part,
                parts: param.parts,
            }),
            cur_param: state.cur_param,
            intermediate: state.intermediate,
        })
    }
}
