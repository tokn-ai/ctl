use serde::{Deserialize, Serialize};

use super::{BufferType, Cursor, CursorKeysMode, DirtyLines, SavedCtx, Terminal};
use crate::buffer::state::BufferState;
use crate::charset::Charset;
use crate::pen::Pen;
use crate::state::{
    validate_pen, validate_scrollback_limit, validate_size, StateError, StateLimits,
};
use crate::tabs::Tabs;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalState {
    cols: usize,
    rows: usize,
    buffer: BufferState,
    other_buffer: BufferState,
    active_buffer_type: BufferType,
    scrollback_limit: Option<usize>,
    cursor: Cursor,
    pen: Pen,
    charsets: [Charset; 2],
    active_charset: usize,
    tabs: Vec<usize>,
    insert_mode: bool,
    origin_mode: bool,
    auto_wrap_mode: bool,
    new_line_mode: bool,
    cursor_keys_mode: CursorKeysMode,
    top_margin: usize,
    bottom_margin: usize,
    saved_ctx: SavedCtx,
    alternate_saved_ctx: SavedCtx,
    dirty_lines: Vec<bool>,
    xtwinops: bool,
    reflow_cursor_line: bool,
}

impl TerminalState {
    pub(crate) fn validate(&self, limits: &StateLimits) -> Result<(), StateError> {
        validate_size(self.cols, self.rows, limits)?;
        validate_scrollback_limit(self.scrollback_limit, limits)?;
        self.validate_aggregate_size(limits)?;
        self.buffer.validate(limits)?;
        self.other_buffer.validate(limits)?;
        if self.buffer.cols != self.cols || self.buffer.rows != self.rows {
            return Err(StateError::InvalidState(
                "active buffer dimensions differ from terminal",
            ));
        }
        // The inactive buffer keeps its previous dimensions until selected and reflowed.
        let (primary, alternate) = match self.active_buffer_type {
            BufferType::Primary => (&self.buffer, &self.other_buffer),
            BufferType::Alternate => (&self.other_buffer, &self.buffer),
        };
        if primary.scrollback_limit != self.scrollback_limit
            || alternate.scrollback_limit != Some(0)
        {
            return Err(StateError::InvalidState("buffer scrollback policy"));
        }
        if self.cursor.col > self.cols || self.cursor.row >= self.rows {
            return Err(StateError::InvalidState("cursor outside terminal"));
        }
        if self.top_margin > self.bottom_margin || self.bottom_margin >= self.rows {
            return Err(StateError::InvalidState("scrolling margins"));
        }
        if self.active_charset >= self.charsets.len() {
            return Err(StateError::InvalidState("active character set"));
        }
        if self.dirty_lines.len() != self.rows {
            return Err(StateError::InvalidState("dirty line count"));
        }
        if self.tabs.len() > self.cols
            || self.tabs.iter().any(|&tab| tab > self.cols)
            || self.tabs.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(StateError::InvalidState("tab stops"));
        }
        validate_pen(&self.pen)?;
        validate_saved_ctx(&self.saved_ctx, &self.buffer)?;
        validate_saved_ctx(&self.alternate_saved_ctx, &self.other_buffer)
    }

    fn validate_aggregate_size(&self, limits: &StateLimits) -> Result<(), StateError> {
        let lines = self
            .buffer
            .line_count()
            .checked_add(self.other_buffer.line_count())
            .ok_or(StateError::LimitExceeded("line count"))?;
        if lines > limits.max_lines || lines > isize::MAX as usize {
            return Err(StateError::LimitExceeded("line count"));
        }
        let cells = self
            .buffer
            .cell_count()?
            .checked_add(self.other_buffer.cell_count()?)
            .ok_or(StateError::LimitExceeded("cell count"))?;
        if cells > limits.max_cells || cells > isize::MAX as usize {
            return Err(StateError::LimitExceeded("cell count"));
        }
        Ok(())
    }

    /// Call only after validation of this entire state.
    pub(crate) fn into_terminal(self) -> Terminal {
        Terminal {
            cols: self.cols,
            rows: self.rows,
            buffer: self.buffer.into_buffer(),
            other_buffer: self.other_buffer.into_buffer(),
            active_buffer_type: self.active_buffer_type,
            scrollback_limit: self.scrollback_limit,
            cursor: self.cursor,
            pen: self.pen,
            charsets: self.charsets,
            active_charset: self.active_charset,
            tabs: Tabs::from_state(self.tabs),
            insert_mode: self.insert_mode,
            origin_mode: self.origin_mode,
            auto_wrap_mode: self.auto_wrap_mode,
            new_line_mode: self.new_line_mode,
            cursor_keys_mode: self.cursor_keys_mode,
            top_margin: self.top_margin,
            bottom_margin: self.bottom_margin,
            saved_ctx: self.saved_ctx,
            alternate_saved_ctx: self.alternate_saved_ctx,
            dirty_lines: DirtyLines::from_state(self.dirty_lines),
            xtwinops: self.xtwinops,
            reflow_cursor_line: self.reflow_cursor_line,
        }
    }
}

fn validate_saved_ctx(ctx: &SavedCtx, buffer: &BufferState) -> Result<(), StateError> {
    if ctx.cursor_col >= buffer.cols || ctx.cursor_row >= buffer.rows {
        return Err(StateError::InvalidState("saved cursor outside its buffer"));
    }
    validate_pen(&ctx.pen)
}

impl Terminal {
    pub(crate) fn export_state(&self) -> TerminalState {
        TerminalState {
            cols: self.cols,
            rows: self.rows,
            buffer: self.buffer.export_state(),
            other_buffer: self.other_buffer.export_state(),
            active_buffer_type: self.active_buffer_type,
            scrollback_limit: self.scrollback_limit,
            cursor: self.cursor,
            pen: self.pen,
            charsets: self.charsets,
            active_charset: self.active_charset,
            tabs: self.tabs.export_state(),
            insert_mode: self.insert_mode,
            origin_mode: self.origin_mode,
            auto_wrap_mode: self.auto_wrap_mode,
            new_line_mode: self.new_line_mode,
            cursor_keys_mode: self.cursor_keys_mode,
            top_margin: self.top_margin,
            bottom_margin: self.bottom_margin,
            saved_ctx: self.saved_ctx.clone(),
            alternate_saved_ctx: self.alternate_saved_ctx.clone(),
            dirty_lines: self.dirty_lines.export_state(),
            xtwinops: self.xtwinops,
            reflow_cursor_line: self.reflow_cursor_line,
        }
    }
}
