use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

use super::Buffer;
use crate::cell::{Cell, Occupancy};
use crate::line::Line;
use crate::pen::Pen;
use crate::state::{
    validate_pen, validate_scrollback_limit, validate_size, StateError, StateLimits,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BufferState {
    pub cols: usize,
    pub rows: usize,
    pub scrollback_limit: Option<usize>,
    lines: VecDeque<LineState>,
    trim_needed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineState {
    cells: Vec<CellState>,
    wrapped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CellState {
    ch: char,
    occupancy: Occupancy,
    pen: Pen,
}

impl BufferState {
    pub(crate) fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub(crate) fn cell_count(&self) -> Result<usize, StateError> {
        self.lines.iter().try_fold(0_usize, |total, line| {
            total
                .checked_add(line.cells.len())
                .ok_or(StateError::LimitExceeded("cell count"))
        })
    }

    pub(crate) fn validate(&self, limits: &StateLimits) -> Result<(), StateError> {
        validate_size(self.cols, self.rows, limits)?;
        validate_scrollback_limit(self.scrollback_limit, limits)?;
        if self.lines.len() < self.rows {
            return Err(StateError::InvalidState("buffer has fewer lines than rows"));
        }
        if self.lines.back().is_some_and(|line| line.wrapped) {
            return Err(StateError::InvalidState("last buffer line is wrapped"));
        }
        for line in &self.lines {
            line.validate(self.cols)?;
        }
        Ok(())
    }

    /// Call only after validation of this buffer and the aggregate terminal bounds.
    pub(crate) fn into_buffer(self) -> Buffer {
        Buffer {
            cols: self.cols,
            rows: self.rows,
            scrollback_limit: self.scrollback_limit,
            trim_needed: self.trim_needed,
            lines: self
                .lines
                .into_iter()
                .map(|line| Line {
                    wrapped: line.wrapped,
                    cells: line
                        .cells
                        .into_iter()
                        .map(|cell| Cell::new(cell.ch, cell.occupancy, cell.pen))
                        .collect(),
                })
                .collect(),
        }
    }
}

impl LineState {
    fn validate(&self, cols: usize) -> Result<(), StateError> {
        if self.cells.len() != cols {
            return Err(StateError::InvalidState("line width does not match buffer"));
        }
        for (index, cell) in self.cells.iter().enumerate() {
            validate_pen(&cell.pen)?;
            match cell.occupancy {
                Occupancy::Single if cell.ch > '\u{7e}' && cell.ch.width() == Some(2) => {
                    return Err(StateError::InvalidState(
                        "wide character has single occupancy",
                    ));
                }
                Occupancy::WideHead => {
                    let tail = self
                        .cells
                        .get(index + 1)
                        .ok_or(StateError::InvalidState("wide head has no continuation"))?;
                    if cell.ch.width() != Some(2) || tail.occupancy != Occupancy::WideTail {
                        return Err(StateError::InvalidState("invalid wide head"));
                    }
                }
                Occupancy::WideTail => {
                    let head = index
                        .checked_sub(1)
                        .and_then(|previous| self.cells.get(previous))
                        .ok_or(StateError::InvalidState("wide continuation has no head"))?;
                    if head.occupancy != Occupancy::WideHead
                        || cell.ch != ' '
                        || head.pen != cell.pen
                    {
                        return Err(StateError::InvalidState("invalid wide continuation"));
                    }
                }
                Occupancy::Single => {}
            }
        }
        Ok(())
    }
}

impl Buffer {
    pub(crate) fn export_state(&self) -> BufferState {
        BufferState {
            cols: self.cols,
            rows: self.rows,
            scrollback_limit: self.scrollback_limit,
            trim_needed: self.trim_needed,
            lines: self
                .lines
                .iter()
                .map(|line| LineState {
                    wrapped: line.wrapped,
                    cells: line
                        .cells
                        .iter()
                        .map(|cell| CellState {
                            ch: cell.char(),
                            occupancy: cell.occupancy(),
                            pen: *cell.pen(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}
