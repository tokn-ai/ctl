use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
  pub row: usize,
  pub column: usize,
}

pub enum Action {
  Stay,
  Close,
  Copy(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BottomBehavior {
  Stay,
  ReturnToLive,
}

#[derive(Default)]
enum DragState {
  #[default]
  Idle,
  Pressed {
    column: usize,
    row: usize,
    anchor: Position,
  },
  Selecting,
}

#[derive(Clone, Copy)]
enum Selection {
  Extending { anchor: Position },
  Complete { anchor: Position, end: Position },
}

/// Immutable styled rows: output and checkpoint replacement cannot move a selection.
pub struct CopyMode {
  pub lines: Vec<Vec<char>>,
  pens: Vec<Vec<(std::ops::Range<usize>, avt::Pen)>>,
  soft_wrapped: Vec<bool>,
  pub cursor: Position,
  pub top: usize,
  pub left: usize,
  selection: Option<Selection>,
  drag_state: DragState,
  query: String,
  searching: bool,
  backwards: bool,
  pub notice: String,
  pub history_gap: bool,
  pub bottom_behavior: BottomBehavior,
}

impl CopyMode {
  pub fn new(lines: Vec<String>) -> Self {
    let lines: Vec<Vec<char>> = lines
      .into_iter()
      .map(|line| line.chars().collect())
      .collect();
    Self {
      cursor: Position {
        row: lines.len().saturating_sub(1),
        column: 0,
      },
      soft_wrapped: vec![false; lines.len()],
      pens: vec![Vec::new(); lines.len()],
      lines,
      top: 0,
      left: 0,
      selection: None,
      drag_state: DragState::Idle,
      query: String::new(),
      searching: false,
      backwards: false,
      notice: String::new(),
      history_gap: false,
      bottom_behavior: BottomBehavior::Stay,
    }
  }

  /// Freeze logical lines into physical rows at the pane's current width.
  pub fn new_wrapped(lines: Vec<String>, width: usize) -> Self {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut soft_wrapped = Vec::new();
    for line in lines {
      let mut row = String::new();
      let mut cells = 0_usize;
      for ch in line.chars() {
        let character_width = Self::width(ch);
        if character_width > 0 && cells > 0 && cells.saturating_add(character_width) > width {
          rows.push(std::mem::take(&mut row));
          soft_wrapped.push(true);
          cells = 0;
        }
        row.push(ch);
        cells = cells.saturating_add(character_width);
      }
      rows.push(row);
      soft_wrapped.push(false);
    }
    let mut mode = Self::new(rows);
    mode.soft_wrapped = soft_wrapped;
    mode
  }

  /// Keep active-buffer row coordinates exact, wrapping only the legacy logical prefix.
  pub fn from_rows(
    prefix: Vec<String>,
    rows: Vec<ctmux_proto::TerminalHistoryRow>,
    width: usize,
  ) -> Self {
    let mut mode = Self::new_wrapped(prefix, width);
    for row in rows {
      let mut pens = Vec::new();
      if !row.styles.is_empty() && ctmux_proto::validate_history_row(&row).is_ok() {
        for span in &row.styles {
          let start = span.start as usize;
          let end = start + span.length as usize;
          pens.push((start..end, ctmux_core::history::style_pen(&span.style)));
        }
      }
      mode.lines.push(row.text.chars().collect());
      mode.pens.push(pens);
      mode.soft_wrapped.push(row.wrapped);
    }
    mode.cursor.row = mode.lines.len().saturating_sub(1);
    mode
  }

  pub fn pen(&self, position: Position) -> avt::Pen {
    let Some(row) = self.pens.get(position.row) else {
      return avt::Pen::default();
    };
    let index = row.partition_point(|(range, _)| range.end <= position.column);
    row
      .get(index)
      .filter(|(range, _)| range.contains(&position.column))
      .map_or_else(avt::Pen::default, |(_, pen)| *pen)
  }

  /// Visible physical rows in the frozen snapshot, including its live screen.
  pub fn history_status(&self, first_visible: usize, height: usize) -> String {
    let total = self.lines.len();
    let start = self.top.saturating_add(first_visible).min(total);
    let end = start.saturating_add(height).min(total);
    let first = if end > start { start + 1 } else { 0 };
    format!(
      "history {}–{} / {}",
      format_line_count(first),
      format_line_count(if first == 0 { 0 } else { end }),
      format_line_count(total)
    )
  }

  pub fn width(ch: char) -> usize {
    ch.width().unwrap_or(0)
  }

  pub fn cursor_column(&self) -> usize {
    self.lines.get(self.cursor.row).map_or(0, |line| {
      line
        .iter()
        .take(self.cursor.column)
        .map(|ch| Self::width(*ch))
        .sum()
    })
  }

  pub fn fit(&mut self, width: usize, height: usize) {
    self.cursor.row = self.cursor.row.min(self.lines.len().saturating_sub(1));
    self.cursor.column = self.cursor.column.min(self.line_len().saturating_sub(1));
    self.top = self.top.min(self.cursor.row);
    if self.cursor.row >= self.top + height.max(1) {
      self.top = self.cursor.row + 1 - height.max(1);
    }
    let column = self.cursor_column();
    let cursor_width = self
      .lines
      .get(self.cursor.row)
      .and_then(|line| line.get(self.cursor.column))
      .map_or(1, |ch| Self::width(*ch).max(1));
    self.left = self.left.min(column);
    if column + cursor_width > self.left + width.max(1) {
      self.left = column + cursor_width - width.max(1);
    }
  }

  pub fn scroll(&mut self, up: bool, rows: usize, height: usize) -> Action {
    let relative = self.cursor.row.saturating_sub(self.top);
    self.top = if up {
      self.top.saturating_sub(rows)
    } else {
      self.top.saturating_add(rows)
    }
    .min(self.lines.len().saturating_sub(height.max(1)));
    self.cursor.row = (self.top + relative).min(self.lines.len().saturating_sub(1));
    self.cursor.column = self.cursor.column.min(self.line_len().saturating_sub(1));
    if !up
      && self.bottom_behavior == BottomBehavior::ReturnToLive
      && self.selection.is_none()
      && !self.searching
      && self.top == self.lines.len().saturating_sub(height.max(1))
    {
      return Action::Close;
    }
    Action::Stay
  }

  fn line_len(&self) -> usize {
    self.lines.get(self.cursor.row).map_or(0, Vec::len)
  }

  /// Map a viewport cell to the character occupying it, including either cell of a wide character.
  fn position_at(&self, column: usize, row: usize) -> Position {
    let row = self
      .top
      .saturating_add(row)
      .min(self.lines.len().saturating_sub(1));
    let Some(line) = self.lines.get(row) else {
      return Position::default();
    };
    let target = self.left.saturating_add(column);
    let mut cell = 0_usize;
    let mut last = 0;
    for (index, ch) in line.iter().enumerate() {
      let width = Self::width(*ch);
      if width == 0 {
        continue;
      }
      last = index;
      cell = cell.saturating_add(width);
      if target < cell {
        return Position { row, column: index };
      }
    }
    Position { row, column: last }
  }

  pub fn begin_drag(&mut self, column: usize, row: usize) {
    self.selection = None;
    self.drag_state = DragState::Pressed {
      column,
      row,
      anchor: self.position_at(column, row),
    };
    self.searching = false;
    self.notice.clear();
  }

  pub fn drag(&mut self, column: usize, row: usize) {
    match self.drag_state {
      DragState::Idle => return,
      DragState::Pressed {
        column: initial_column,
        row: initial_row,
        ..
      } if column == initial_column && row == initial_row => return,
      DragState::Pressed { anchor, .. } => {
        self.selection = Some(Selection::Extending { anchor });
        self.drag_state = DragState::Selecting;
      }
      DragState::Selecting => {}
    }
    self.cursor = self.position_at(column, row);
  }

  pub fn finish_drag(&mut self) -> bool {
    match std::mem::take(&mut self.drag_state) {
      DragState::Selecting => {
        self.complete_selection();
        true
      }
      DragState::Pressed { .. } => {
        self.selection = None;
        false
      }
      DragState::Idle => false,
    }
  }

  pub fn release_drag(&mut self, behavior: crate::MouseRelease) -> Action {
    if self.finish_drag() && behavior == crate::MouseRelease::Copy {
      self.copy_selection()
    } else {
      Action::Stay
    }
  }

  pub fn clear_selection(&mut self) {
    self.selection = None;
    self.drag_state = DragState::Idle;
    self.notice.clear();
  }

  fn copy_selection(&mut self) -> Action {
    if let Some(text) = self.selected_text().filter(|text| !text.is_empty()) {
      self.complete_selection();
      Action::Copy(text)
    } else {
      Action::Stay
    }
  }

  fn complete_selection(&mut self) {
    if let Some(Selection::Extending { anchor }) = self.selection {
      self.selection = Some(Selection::Complete {
        anchor,
        end: self.cursor,
      });
    }
  }

  fn selection_bounds(&self) -> Option<(Position, Position)> {
    let (anchor, end) = match self.selection? {
      Selection::Extending { anchor } => (anchor, self.cursor),
      Selection::Complete { anchor, end } => (anchor, end),
    };
    Some((anchor.min(end), anchor.max(end)))
  }

  pub fn selected(&self, position: Position) -> bool {
    self
      .selection_bounds()
      .is_some_and(|(start, end)| (start..=end).contains(&position))
  }

  fn selected_text(&self) -> Option<String> {
    let (start, end) = self.selection_bounds()?;
    let mut selected = String::new();
    for row in start.row..=end.row {
      let line = self.lines.get(row)?;
      let from = if row == start.row {
        start.column.min(line.len())
      } else {
        0
      };
      let to = if row == end.row {
        let mut to = end.column.saturating_add(1).min(line.len());
        // A combining mark occupies its base character's cell and belongs in the copied text.
        while line.get(to).is_some_and(|ch| Self::width(*ch) == 0) {
          to += 1;
        }
        to
      } else {
        line.len()
      };
      selected.extend(line[from..to].iter());
      if row != end.row && !self.soft_wrapped.get(row).copied().unwrap_or(false) {
        selected.push('\n');
      }
    }
    Some(selected)
  }

  pub fn key(&mut self, key: KeyEvent, page: usize) -> Action {
    self.finish_drag();
    if self.searching {
      match key.code {
        KeyCode::Esc => self.searching = false,
        KeyCode::Enter => {
          self.searching = false;
          self.search(self.backwards);
        }
        KeyCode::Backspace => {
          self.query.pop();
        }
        KeyCode::Char(ch)
          if !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
          self.query.push(ch);
        }
        _ => {}
      }
      return Action::Stay;
    }
    self.notice.clear();
    let code = if key.modifiers.contains(KeyModifiers::CONTROL) {
      match key.code {
        KeyCode::Char('c') => return Action::Close,
        KeyCode::Char('g') => {
          self.clear_selection();
          return Action::Stay;
        }
        KeyCode::Char('b') => KeyCode::Left,
        KeyCode::Char('f') => KeyCode::Right,
        KeyCode::Char('p') => KeyCode::Up,
        KeyCode::Char('n') => KeyCode::Down,
        KeyCode::Char('a') => KeyCode::Home,
        KeyCode::Char('e') => KeyCode::End,
        KeyCode::Char('v') => KeyCode::PageDown,
        KeyCode::Char(' ' | '@') | KeyCode::Null => KeyCode::Char(' '),
        KeyCode::Char('w') => KeyCode::Enter,
        KeyCode::Char('r') => KeyCode::Char('?'),
        KeyCode::Char('s') => KeyCode::Char('/'),
        _ => return Action::Stay,
      }
    } else if key.modifiers.contains(KeyModifiers::ALT) {
      match key.code {
        KeyCode::Char('v') => KeyCode::PageUp,
        KeyCode::Char('w') => KeyCode::Enter,
        KeyCode::Char('<') => KeyCode::Char('g'),
        KeyCode::Char('>') => KeyCode::Char('G'),
        _ => return Action::Stay,
      }
    } else if key.code == KeyCode::Char(' ') {
      KeyCode::PageDown
    } else {
      key.code
    };
    match code {
      KeyCode::Esc | KeyCode::Char('q') => return Action::Close,
      KeyCode::Up | KeyCode::Char('k') => self.cursor.row = self.cursor.row.saturating_sub(1),
      KeyCode::Down | KeyCode::Char('j') => self.cursor.row = self.cursor.row.saturating_add(1),
      KeyCode::Left | KeyCode::Char('h') => {
        self.cursor.column = self.cursor.column.saturating_sub(1);
      }
      KeyCode::Right | KeyCode::Char('l') => {
        self.cursor.column = self.cursor.column.saturating_add(1);
      }
      KeyCode::PageUp => self.cursor.row = self.cursor.row.saturating_sub(page.max(1)),
      KeyCode::PageDown => self.cursor.row = self.cursor.row.saturating_add(page.max(1)),
      KeyCode::Char('g') => self.cursor = Position::default(),
      KeyCode::Char('G') => self.cursor.row = self.lines.len().saturating_sub(1),
      KeyCode::Home | KeyCode::Char('0') => self.cursor.column = 0,
      KeyCode::End | KeyCode::Char('$') => self.cursor.column = self.line_len().saturating_sub(1),
      KeyCode::Char(' ' | 'v') => {
        self.selection = Some(Selection::Extending {
          anchor: self.cursor,
        });
      }
      KeyCode::Enter | KeyCode::Char('y') => {
        let action = self.copy_selection();
        if matches!(action, Action::Copy(_)) {
          return action;
        }
        self.notice = "Ctrl+Space or v starts a selection".into();
      }
      KeyCode::Char('/' | '?') => {
        self.backwards = code == KeyCode::Char('?');
        self.searching = true;
        self.query.clear();
      }
      KeyCode::Char('n') => self.search(self.backwards),
      KeyCode::Char('N') => self.search(!self.backwards),
      _ => {}
    }
    self.cursor.row = self.cursor.row.min(self.lines.len().saturating_sub(1));
    self.cursor.column = self.cursor.column.min(self.line_len().saturating_sub(1));
    Action::Stay
  }

  fn search(&mut self, backwards: bool) {
    let needle: Vec<char> = self.query.chars().collect();
    if needle.is_empty() {
      return;
    }
    let mut first = None;
    let mut last = None;
    let mut next = None;
    let mut previous = None;
    let mut text = Vec::new();
    let mut positions = Vec::new();
    for (row, line) in self.lines.iter().enumerate() {
      text.extend(line.iter().copied());
      positions.extend((0..line.len()).map(|column| Position { row, column }));
      if self.soft_wrapped.get(row).copied().unwrap_or(false) && row + 1 < self.lines.len() {
        continue;
      }
      for (column, candidate) in text.windows(needle.len()).enumerate() {
        if candidate == needle {
          let position = positions[column];
          first.get_or_insert(position);
          last = Some(position);
          if position > self.cursor {
            next.get_or_insert(position);
          }
          if position < self.cursor {
            previous = Some(position);
          }
        }
      }
      text.clear();
      positions.clear();
    }
    if let Some(position) = if backwards {
      previous.or(last)
    } else {
      next.or(first)
    } {
      self.cursor = position;
    } else {
      self.notice = "No match".into();
    }
  }

  pub fn status(&self) -> String {
    if self.searching {
      return format!("{}{}", if self.backwards { '?' } else { '/' }, self.query);
    }
    format!(
      " COPY {}/{}{}{} | arrows/PgUp/PgDn | Ctrl+Space select, Alt+w copy | v/y vi | /? search | q exit",
      self.cursor.row + 1,
      self.lines.len(),
      if self.history_gap {
        " | Snapshot incomplete"
      } else {
        ""
      },
      if self.notice.is_empty() {
        String::new()
      } else {
        format!(" | {}", self.notice)
      }
    )
  }
}

pub fn format_line_count(count: usize) -> String {
  let digits = count.to_string();
  let mut formatted = String::new();
  for (index, digit) in digits.chars().enumerate() {
    if index > 0 && (digits.len() - index).is_multiple_of(3) {
      formatted.push(',');
    }
    formatted.push(digit);
  }
  formatted
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn history_range_counts_visible_rows_and_handles_empty_and_clipped_views() {
    let mut mode = CopyMode::new((0..1240).map(|row| row.to_string()).collect());
    mode.top = 200;
    assert_eq!(mode.history_status(0, 40), "history 201–240 / 1,240");
    assert_eq!(mode.history_status(5, 10), "history 206–215 / 1,240");
    mode.top = 1235;
    assert_eq!(mode.history_status(0, 40), "history 1,236–1,240 / 1,240");
    assert_eq!(mode.history_status(0, 0), "history 0–0 / 1,240");
    assert_eq!(
      CopyMode::new(Vec::new()).history_status(0, 20),
      "history 0–0 / 0"
    );
  }

  #[test]
  fn copying_styled_unicode_retains_text_without_ansi() {
    let mut vt = avt::Vt::new(8, 1);
    vt.feed_str("\x1b[38;2;1;2;3m界X\x1b[0m");
    let row = ctmux_core::history::capture_row(vt.line(0), false);
    let mut mode = CopyMode::from_rows(Vec::new(), vec![row], 8);
    mode.begin_drag(0, 0);
    mode.drag(2, 0);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("expected copied selection")
    };
    assert_eq!(text, "界X");
    assert_eq!(
      mode.pen(Position { row: 0, column: 1 }).foreground(),
      Some(avt::Color::rgb(1, 2, 3))
    );
  }

  #[test]
  fn physical_rows_preserve_original_width_and_wrap_flags_after_a_logical_prefix() {
    let mut mode = CopyMode::from_rows(
      vec!["older".into()],
      vec![
        ctmux_proto::TerminalHistoryRow {
          styles: Vec::new(),
          text: "abcdef".into(),
          wrapped: true,
        },
        ctmux_proto::TerminalHistoryRow {
          styles: Vec::new(),
          text: "界x".into(),
          wrapped: false,
        },
      ],
      3,
    );
    assert_eq!(
      mode
        .lines
        .iter()
        .map(|row| row.iter().collect::<String>())
        .collect::<Vec<_>>(),
      ["old", "er", "abcdef", "界x"]
    );
    assert_eq!(mode.soft_wrapped, [true, false, true, false]);
    mode.begin_drag(1, 2);
    mode.drag(2, 3);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("physical rows must retain their original continuation boundary");
    };
    assert_eq!(text, "bcdef界x");
    mode.query = "def界".into();
    mode.search(false);
    assert_eq!((mode.cursor.row, mode.cursor.column), (2, 3));
    mode.search(true);
    assert_eq!((mode.cursor.row, mode.cursor.column), (2, 3));
  }

  #[test]
  fn physical_rows_keep_blank_grid_rows_in_place() {
    let mut mode = CopyMode::from_rows(
      Vec::new(),
      ["ab", "", "", "cd"]
        .into_iter()
        .map(|text| ctmux_proto::TerminalHistoryRow {
          styles: Vec::new(),
          text: text.into(),
          wrapped: false,
        })
        .collect(),
      20,
    );
    assert_eq!(mode.lines.len(), 4);
    assert_eq!(mode.cursor.row, 3);
    mode.begin_drag(0, 0);
    mode.drag(1, 3);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("blank grid rows must survive a multiline selection");
    };
    assert_eq!(text, "ab\n\n\ncd");
  }

  #[test]
  fn wrapped_rows_copy_without_inserting_newlines_at_soft_wraps() {
    let mut mode = CopyMode::new_wrapped(vec!["abcdefghij".into()], 4);
    assert_eq!(
      mode
        .lines
        .iter()
        .map(|row| row.iter().collect::<String>())
        .collect::<Vec<_>>(),
      ["abcd", "efgh", "ij"]
    );
    assert_eq!(mode.soft_wrapped, [true, true, false]);
    mode.begin_drag(0, 0);
    mode.drag(1, 2);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("wrapped rows must copy their original text");
    };
    assert_eq!(text, "abcdefghij");
  }

  #[test]
  fn fitting_the_cursor_uses_its_actual_width_at_the_right_edge() {
    let mut mode = CopyMode::new(vec!["abc界".into()]);
    mode.cursor.column = 2;
    mode.fit(3, 1);
    assert_eq!(mode.left, 0);
    mode.cursor.column = 3;
    mode.fit(4, 1);
    assert_eq!(mode.left, 1);
    let mut mode = CopyMode::new_wrapped(vec!["abc界".into()], 4);
    assert_eq!(mode.lines, [vec!['a', 'b', 'c'], vec!['界']]);
    mode.begin_drag(0, 1);
    mode.fit(4, 2);
    assert_eq!(mode.left, 0);
  }

  #[test]
  fn searching_soft_wraps_maps_matches_to_physical_positions_in_both_directions() {
    let mut mode = CopyMode::new_wrapped(vec!["界xneedle---needle".into()], 4);
    mode.query = "needle".into();
    mode.search(false);
    assert_eq!((mode.cursor.row, mode.cursor.column), (0, 2));
    assert_eq!(mode.cursor_column(), 3);
    mode.search(false);
    assert_eq!((mode.cursor.row, mode.cursor.column), (3, 0));
    mode.search(true);
    assert_eq!((mode.cursor.row, mode.cursor.column), (0, 2));
    mode.search(true);
    assert_eq!((mode.cursor.row, mode.cursor.column), (3, 0));
    let mut mode = CopyMode::new_wrapped(vec!["nee".into(), "dle".into()], 4);
    mode.query = "needle".into();
    mode.search(false);
    assert_eq!(mode.notice, "No match");
  }

  #[test]
  fn wrapped_rows_preserve_explicit_newlines_blank_lines_and_unicode_clusters() {
    let mut mode = CopyMode::new_wrapped(
      vec!["ab界e\u{301}fg".into(), String::new(), "界終".into()],
      3,
    );
    assert_eq!(
      mode
        .lines
        .iter()
        .map(|row| row.iter().collect::<String>())
        .collect::<Vec<_>>(),
      ["ab", "界e\u{301}", "fg", "", "界", "終"]
    );
    assert_eq!(mode.soft_wrapped, [true, true, false, false, true, false]);
    mode.begin_drag(0, 0);
    mode.drag(0, 5);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("explicit line breaks must survive copying wrapped rows");
    };
    assert_eq!(text, "ab界e\u{301}fg\n\n界終");
  }

  #[test]
  fn resizing_keeps_wrapped_rows_and_selection_frozen() {
    let mut mode = CopyMode::new_wrapped(vec!["abcdefghij".into()], 4);
    let rows = mode.lines.clone();
    mode.begin_drag(2, 0);
    mode.drag(0, 2);
    mode.fit(2, 1);
    assert_eq!(mode.lines, rows);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("resizing must preserve a wrapped selection");
    };
    assert_eq!(text, "cdefghi");
  }

  #[test]
  fn logical_constructor_retains_horizontal_lines_and_zero_width_wrapping_is_safe() {
    let mode = CopyMode::new(vec!["abcdefghij".into(), String::new()]);
    assert_eq!(mode.lines.len(), 2);
    assert_eq!(mode.lines[0].len(), 10);
    assert_eq!(mode.soft_wrapped, [false, false]);
    let mode = CopyMode::new_wrapped(vec!["ab".into()], 0);
    assert_eq!(mode.lines, [vec!['a'], vec!['b']]);
  }

  #[test]
  fn mouse_drag_maps_scrolled_cells_to_wide_characters_and_combining_marks() {
    let mut mode = CopyMode::new(vec!["older".into(), "a界e\u{301}z".into()]);
    mode.top = 1;
    mode.left = 1;
    mode.begin_drag(1, 0);
    mode.drag(2, 0);
    assert_eq!(mode.cursor.column, 2);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("explicit copy must use the completed drag");
    };
    assert_eq!(text, "界e\u{301}");
  }

  #[test]
  fn mouse_drag_in_reverse_preserves_multiline_text_and_empty_lines() {
    let mut mode = CopyMode::new(vec!["alpha".into(), String::new(), "界e\u{301}last".into()]);
    mode.begin_drag(2, 2);
    mode.drag(1, 0);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("reverse explicit copy must use the completed drag");
    };
    assert_eq!(text, "lpha\n\n界e\u{301}");
  }

  #[test]
  fn mouse_drag_within_a_wide_character_copies_the_whole_character() {
    let mut mode = CopyMode::new(vec!["界x".into()]);
    mode.begin_drag(0, 0);
    mode.drag(1, 0);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("moving across a wide character must count as a drag");
    };
    assert_eq!(text, "界");
  }

  #[test]
  fn mouse_click_does_not_copy_and_inactive_drag_does_not_move_the_cursor() {
    let mut mode = CopyMode::new(vec!["abc".into()]);
    let original_cursor = (mode.cursor.row, mode.cursor.column);
    mode.begin_drag(1, 0);
    mode.drag(1, 0);
    mode.finish_drag();
    assert!(mode.selection.is_none());
    mode.drag(2, 0);
    assert_eq!((mode.cursor.row, mode.cursor.column), original_cursor);
    mode.finish_drag();
  }

  #[test]
  fn configured_mouse_copy_only_fires_once_for_a_completed_drag() {
    let mut mode = CopyMode::new(vec!["abcde".into()]);
    assert!(matches!(
      mode.release_drag(crate::MouseRelease::Copy),
      Action::Stay
    ));
    mode.begin_drag(0, 0);
    mode.drag(1, 0);
    let Action::Copy(text) = mode.release_drag(crate::MouseRelease::Copy) else {
      panic!("configured mouse copy must copy the completed selection");
    };
    assert_eq!(text, "ab");
    assert!(matches!(
      mode.release_drag(crate::MouseRelease::Copy),
      Action::Stay
    ));
    mode.begin_drag(1, 0);
    mode.drag(1, 0);
    assert!(matches!(
      mode.release_drag(crate::MouseRelease::Copy),
      Action::Stay
    ));
    assert!(mode.selected_text().is_none());
  }

  #[test]
  fn finishing_a_drag_for_keyboard_navigation_does_not_trigger_automatic_copy() {
    let mut mode = CopyMode::new(vec!["abcde".into()]);
    mode.begin_drag(0, 0);
    mode.drag(1, 0);
    assert!(matches!(mode.key(key(KeyCode::Right), 5), Action::Stay));
    assert!(matches!(
      mode.release_drag(crate::MouseRelease::Copy),
      Action::Stay
    ));
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 5) else {
      panic!("keyboard navigation must retain the completed selection");
    };
    assert_eq!(text, "ab");
  }

  #[test]
  fn copying_empty_text_is_inert_but_whitespace_and_newlines_are_preserved() {
    let mut mode = CopyMode::new(vec![String::new(), String::new(), "  ".into()]);
    mode.begin_drag(0, 0);
    mode.drag(1, 0);
    assert!(matches!(
      mode.release_drag(crate::MouseRelease::Copy),
      Action::Stay
    ));
    assert!(matches!(mode.key(key(KeyCode::Char('y')), 5), Action::Stay));
    for (start_row, end_row, expected) in [(0, 1, "\n"), (2, 2, "  ")] {
      mode.begin_drag(0, start_row);
      mode.drag(1, end_row);
      let Action::Copy(text) = mode.release_drag(crate::MouseRelease::Copy) else {
        panic!("nonempty whitespace selections must copy");
      };
      assert_eq!(text, expected);
    }
  }

  #[test]
  fn mouse_drag_handles_empty_history_and_clamps_short_lines() {
    let mut mode = CopyMode::new(Vec::new());
    mode.begin_drag(usize::MAX, usize::MAX);
    mode.drag(0, 0);
    mode.finish_drag();
    let mut mode = CopyMode::new(vec![String::new(), "a".into()]);
    mode.begin_drag(0, 0);
    mode.drag(usize::MAX, usize::MAX);
    mode.finish_drag();
    let Action::Copy(text) = mode.key(key(KeyCode::Char('y')), 10) else {
      panic!("dragging beyond a short line must clamp to its end");
    };
    assert_eq!(text, "\na");
  }

  #[test]
  fn emacs_selection_copy_and_space_paging_keep_vi_aliases_available() {
    let mut mode = CopyMode::new(vec!["abc".into(), "next".into(), "last".into()]);
    mode.key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::ALT), 2);
    mode.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL), 2);
    mode.key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 2);
    let Action::Copy(text) = mode.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT), 2)
    else {
      panic!("Alt+w must copy the selected text");
    };
    assert_eq!(text, "ab");
    mode.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL), 2);
    assert!(mode.selection.is_none());
    mode.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), 2);
    assert_eq!(mode.cursor.row, 2);
    assert!(mode.selection.is_none());
    mode.key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), 2);
    assert!(mode.selection.is_some());
  }

  #[test]
  fn completed_mouse_selection_stays_fixed_while_browsing_and_copying() {
    let mut mode = CopyMode::new((0..20).map(|row| format!("line-{row:02}")).collect());
    mode.top = 5;
    mode.begin_drag(1, 1);
    mode.drag(3, 2);
    mode.finish_drag();
    let expected = "ine-06\nline";
    assert_eq!(mode.selected_text().as_deref(), Some(expected));
    assert_eq!(mode.top, 5);
    for event in [
      key(KeyCode::Char('y')),
      key(KeyCode::Enter),
      KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT),
      KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    ] {
      mode.scroll(true, 3, 5);
      mode.key(key(KeyCode::PageDown), 5);
      mode.query = "line-15".into();
      mode.search(false);
      mode.fit(10, 5);
      let viewport = (mode.top, mode.left);
      let Action::Copy(text) = mode.key(event, 5) else {
        panic!("explicit copy must use the completed selection");
      };
      assert_eq!(text, expected);
      assert_eq!((mode.top, mode.left), viewport);
      assert!(mode.selected(Position { row: 6, column: 1 }));
      assert!(mode.selected(Position { row: 7, column: 3 }));
      assert!(!mode.selected(Position { row: 6, column: 0 }));
      assert!(!mode.selected(Position { row: 7, column: 4 }));
    }
  }

  #[test]
  fn keyboard_selection_extends_until_copy_then_new_selection_replaces_it() {
    let mut mode = CopyMode::new(vec!["abcd".into(), "efgh".into()]);
    mode.key(key(KeyCode::Char('g')), 5);
    mode.key(key(KeyCode::Char('v')), 5);
    mode.key(key(KeyCode::Right), 5);
    assert_eq!(mode.selected_text().as_deref(), Some("ab"));
    assert!(matches!(
      mode.key(key(KeyCode::Char('y')), 5),
      Action::Copy(_)
    ));
    mode.key(key(KeyCode::Down), 5);
    mode.scroll(false, 1, 1);
    mode.fit(1, 1);
    assert_eq!(mode.selected_text().as_deref(), Some("ab"));
    mode.key(key(KeyCode::Char('v')), 5);
    mode.key(key(KeyCode::Right), 5);
    assert_eq!(mode.selected_text().as_deref(), Some("fg"));
  }

  #[test]
  fn clearing_completed_selection_keeps_the_viewport() {
    let mut mode = CopyMode::new((0..20).map(|row| format!("line-{row:02}")).collect());
    mode.top = 5;
    for clear_with_click in [false, true] {
      mode.begin_drag(1, 1);
      mode.drag(3, 2);
      mode.finish_drag();
      let viewport = (mode.top, mode.left);
      if clear_with_click {
        mode.begin_drag(0, 1);
        mode.finish_drag();
      } else {
        mode.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL), 5);
      }
      mode.fit(10, 5);
      assert!(mode.selected_text().is_none());
      assert_eq!((mode.top, mode.left), viewport);
    }
  }

  #[test]
  fn clicking_a_short_archive_row_does_not_shift_the_horizontal_view() {
    let mut mode = CopyMode::new(vec!["x".repeat(40), "short".into()]);
    mode.cursor = Position { row: 0, column: 29 };
    mode.fit(10, 2);
    assert_eq!(mode.left, 20);
    mode.begin_drag(2, 0);
    mode.drag(4, 0);
    mode.finish_drag();
    for row in [0, 1] {
      mode.begin_drag(0, row);
      mode.fit(10, 2);
      assert_eq!(mode.left, 20);
      mode.finish_drag();
      mode.fit(10, 2);
      assert_eq!((mode.top, mode.left), (0, 20));
      assert!(mode.selected_text().is_none());
    }
  }

  #[test]
  fn wheel_mode_exits_at_live_output_but_selection_and_keyboard_modes_stay() {
    let mut mode = CopyMode::new((0..20).map(|row| row.to_string()).collect());
    mode.fit(20, 5);
    mode.bottom_behavior = BottomBehavior::ReturnToLive;
    assert!(matches!(mode.scroll(true, 5, 5), Action::Stay));
    assert_eq!(mode.top, 10);
    mode.key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), 5);
    assert!(matches!(mode.scroll(false, 5, 5), Action::Stay));
    mode.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL), 5);
    assert!(matches!(mode.scroll(false, 5, 5), Action::Close));
    mode.bottom_behavior = BottomBehavior::Stay;
    assert!(matches!(mode.scroll(false, 5, 5), Action::Stay));
  }

  #[test]
  fn missing_history_is_visible_without_changing_copied_lines() {
    let mut mode = CopyMode::new(vec!["retained output".into()]);
    mode.history_gap = true;
    assert!(mode.status().starts_with(" COPY 1/1 | Snapshot incomplete"));
    assert_eq!(mode.lines[0].iter().collect::<String>(), "retained output");
  }
  fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
  }

  #[test]
  fn unicode_selection_crosses_logical_lines_without_padding() {
    let mut mode = CopyMode::new(vec!["a界b".into(), "next".into()]);
    mode.key(key(KeyCode::Char('g')), 10);
    mode.key(key(KeyCode::Right), 10);
    assert_eq!(mode.cursor_column(), 1);
    mode.key(key(KeyCode::Char('v')), 10);
    mode.key(key(KeyCode::Down), 10);
    let Action::Copy(text) = mode.key(key(KeyCode::Enter), 10) else {
      panic!("copy");
    };
    assert_eq!(text, "界b\nne");
  }

  #[test]
  fn search_wraps_in_both_directions_and_resize_keeps_cursor_visible() {
    let mut mode = CopyMode::new(vec!["界 needle".into(), "needle".into()]);
    mode.query = "needle".into();
    mode.search(false);
    assert_eq!((mode.cursor.row, mode.cursor.column), (0, 2));
    mode.search(true);
    assert_eq!(mode.cursor.row, 1);
    mode.key(key(KeyCode::End), 1);
    mode.fit(3, 1);
    assert_eq!(mode.top, 1);
    assert!(mode.left > 0);
    mode.query = "absent".into();
    mode.search(false);
    assert_eq!(mode.notice, "No match");
  }
}
