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

/// Immutable logical lines: output and checkpoint replacement cannot move a selection.
pub struct CopyMode {
  pub lines: Vec<Vec<char>>,
  pub cursor: Position,
  pub top: usize,
  pub left: usize,
  anchor: Option<Position>,
  query: String,
  searching: bool,
  backwards: bool,
  pub notice: String,
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
      lines,
      top: 0,
      left: 0,
      anchor: None,
      query: String::new(),
      searching: false,
      backwards: false,
      notice: String::new(),
    }
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
    self.left = self.left.min(column);
    if column + 2 > self.left + width.max(2) {
      self.left = column + 2 - width.max(2);
    }
  }

  fn line_len(&self) -> usize {
    self.lines.get(self.cursor.row).map_or(0, Vec::len)
  }

  pub fn selected(&self, position: Position) -> bool {
    self
      .anchor
      .is_some_and(|anchor| (anchor.min(self.cursor)..=anchor.max(self.cursor)).contains(&position))
  }

  fn selection(&self) -> Option<String> {
    let anchor = self.anchor?;
    let start = anchor.min(self.cursor);
    let end = anchor.max(self.cursor);
    let mut selected = Vec::new();
    for row in start.row..=end.row {
      let line = self.lines.get(row)?;
      let from = if row == start.row {
        start.column.min(line.len())
      } else {
        0
      };
      let to = if row == end.row {
        (end.column + 1).min(line.len())
      } else {
        line.len()
      };
      selected.push(line[from..to].iter().collect::<String>());
    }
    Some(selected.join("\n"))
  }

  pub fn key(&mut self, key: KeyEvent, page: usize) -> Action {
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
    match key.code {
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
      KeyCode::Char(' ' | 'v') => self.anchor = Some(self.cursor),
      KeyCode::Enter | KeyCode::Char('y') => {
        if let Some(text) = self.selection() {
          return Action::Copy(text);
        }
        self.notice = "Space starts a selection".into();
      }
      KeyCode::Char('/' | '?') => {
        self.backwards = key.code == KeyCode::Char('?');
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
    for (row, line) in self.lines.iter().enumerate() {
      for (column, candidate) in line.windows(needle.len()).enumerate() {
        if candidate == needle {
          let position = Position { row, column };
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
      " COPY {}/{} | arrows/PgUp/PgDn g/G | Space select, Enter copy | /? search n/N | q exit {}",
      self.cursor.row + 1,
      self.lines.len(),
      self.notice
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
  }

  #[test]
  fn unicode_selection_crosses_logical_lines_without_padding() {
    let mut mode = CopyMode::new(vec!["a界b".into(), "next".into()]);
    mode.key(key(KeyCode::Char('g')), 10);
    mode.key(key(KeyCode::Right), 10);
    assert_eq!(mode.cursor_column(), 1);
    mode.key(key(KeyCode::Char(' ')), 10);
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
