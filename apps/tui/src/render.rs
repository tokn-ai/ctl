use crate::{
  copy::{CopyMode, Position},
  pane::Pane,
};
use crossterm::{
  cursor::{Hide, MoveTo, Show},
  queue,
  style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
  },
};
use rmux_proto::ViewInfo;
use std::collections::BTreeMap;
use std::io::{self, Write};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Pixel {
  ch: char,
  width: u8,
  pen: avt::Pen,
}

impl From<&avt::Cell> for Pixel {
  fn from(cell: &avt::Cell) -> Self {
    Self {
      ch: cell.char(),
      width: cell.width(),
      pen: *cell.pen(),
    }
  }
}

#[derive(PartialEq, Eq)]
pub struct Frame {
  columns: u16,
  rows: u16,
  cells: Vec<Pixel>,
  cursor: Option<(u16, u16)>,
}

impl Frame {
  pub fn new(columns: u16, rows: u16) -> Self {
    Self {
      columns,
      rows,
      cursor: None,
      cells: vec![
        Pixel {
          ch: ' ',
          width: 1,
          pen: avt::Pen::default()
        };
        usize::from(columns) * usize::from(rows)
      ],
    }
  }

  fn set(&mut self, x: u16, y: u16, pixel: Pixel) {
    if x < self.columns && y < self.rows {
      self.cells[usize::from(y) * usize::from(self.columns) + usize::from(x)] = pixel;
    }
  }

  pub fn text(&mut self, x: u16, y: u16, text: &str, reverse: bool) {
    if x >= self.columns || y >= self.rows {
      return;
    }
    // Parse printable text in a bounded row for consistent Unicode cell widths.
    let mut vt = avt::Vt::new(usize::from(self.columns - x), 1);
    let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
    if reverse {
      vt.feed_str("\x1b[7m\x1b[2K");
    }
    // Disable wrap so an overlong status cannot roll its beginning off the row.
    vt.feed_str("\x1b[?7l");
    vt.feed_str(&text);
    for (column, cell) in vt.line(0).cells().iter().enumerate() {
      self.set(
        x + u16::try_from(column).expect("bounded column"),
        y,
        cell.into(),
      );
    }
  }

  pub fn canvas(&mut self, view: &ViewInfo, panes: &BTreeMap<String, Pane>, focused: &str) {
    let height = self.rows.saturating_sub(1);
    if height == 0 || self.columns == 0 {
      return;
    }
    let (offset_x, offset_y) =
      viewport_offset(view, panes.get(focused), focused, self.columns, height);
    // Only cells not covered by a pane become dividers; no border reduces PTY space.
    let mut covered = vec![false; usize::from(self.columns) * usize::from(height)];
    for rect in &view.panes {
      let pane = panes.get(&rect.terminal_id);
      let lines: Vec<_> = pane.map_or_else(Vec::new, |pane| pane.model.vt.view().collect());
      for row in 0..rect.rows {
        let Some(y) = (rect.top + row)
          .checked_sub(offset_y)
          .filter(|y| *y < height)
        else {
          continue;
        };
        let cells = lines.get(usize::from(row));
        for column in 0..rect.columns {
          let Some(x) = (rect.left + column)
            .checked_sub(offset_x)
            .filter(|x| *x < self.columns)
          else {
            continue;
          };
          covered[usize::from(y) * usize::from(self.columns) + usize::from(x)] = true;
          if let Some(cell) = cells.and_then(|line| line.cells().get(usize::from(column))) {
            let pixel: Pixel = cell.into();
            // Do not emit half of a wide glyph at a viewport edge.
            if (pixel.width == 0 && x == 0)
              || (pixel.width == 2 && (x + 1 >= self.columns || column + 1 >= rect.columns))
            {
              continue;
            }
            self.set(x, y, pixel);
          }
        }
      }
      if let Some(ended) = pane.and_then(|pane| pane.ended.as_deref())
        && let Some(y) = rect.top.checked_sub(offset_y).filter(|y| *y < height)
      {
        let mut label = avt::Vt::new(usize::from(rect.columns.max(1)), 1);
        label.feed_str("\x1b[7m\x1b[?7l");
        label.feed_str(&format!(" {ended} — press a key when focused"));
        for (column, cell) in label.line(0).cells().iter().enumerate() {
          if let Some(x) = (usize::from(rect.left) + column)
            .checked_sub(usize::from(offset_x))
            .filter(|x| *x < usize::from(self.columns))
          {
            self.set(u16::try_from(x).expect("bounded"), y, cell.into());
          }
        }
      }
      if rect.terminal_id == focused
        && let Some(pane) = pane
      {
        let cursor = pane.model.vt.cursor();
        if cursor.visible
          && pane.connected
          && cursor.col < usize::from(rect.columns)
          && cursor.row < usize::from(rect.rows)
        {
          let x = usize::from(rect.left) + cursor.col;
          let y = usize::from(rect.top) + cursor.row;
          if x >= usize::from(offset_x) && y >= usize::from(offset_y) {
            let x = x - usize::from(offset_x);
            let y = y - usize::from(offset_y);
            if x < usize::from(self.columns) && y < usize::from(height) {
              self.cursor = Some((
                u16::try_from(x).expect("bounded"),
                u16::try_from(y).expect("bounded"),
              ));
            }
          }
        }
      }
    }
    self.dividers(view, &covered, (offset_x, offset_y), focused);
  }

  fn dividers(&mut self, view: &ViewInfo, covered: &[bool], offset: (u16, u16), focused: &str) {
    let (offset_x, offset_y) = offset;
    let height = self.rows.saturating_sub(1);
    let mut palette = avt::Vt::new(2, 1);
    palette.feed_str("\x1b[36mX\x1b[90mX");
    let active_pen = *palette.line(0).cells()[0].pen();
    let inactive_pen = *palette.line(0).cells()[1].pen();
    for y in 0..height.min(view.canvas_size.rows.saturating_sub(offset_y)) {
      for x in 0..self
        .columns
        .min(view.canvas_size.columns.saturating_sub(offset_x))
      {
        if !covered[usize::from(y) * usize::from(self.columns) + usize::from(x)] {
          let vertical =
            x > 0 && covered[usize::from(y) * usize::from(self.columns) + usize::from(x - 1)];
          let absolute_x = x + offset_x;
          let absolute_y = y + offset_y;
          let active_edge = view
            .panes
            .iter()
            .find(|pane| pane.terminal_id == focused)
            .is_some_and(|pane| {
              ((absolute_x + 1 == pane.left || absolute_x == pane.left + pane.columns)
                && (pane.top..pane.top + pane.rows).contains(&absolute_y))
                || ((absolute_y + 1 == pane.top || absolute_y == pane.top + pane.rows)
                  && (pane.left..pane.left + pane.columns).contains(&absolute_x))
            });
          let pen = if active_edge {
            active_pen
          } else {
            inactive_pen
          };
          self.set(
            x,
            y,
            Pixel {
              ch: if vertical { '│' } else { '─' },
              width: 1,
              pen,
            },
          );
        }
      }
    }
  }

  pub fn copy_mode(&mut self, mode: &CopyMode) {
    let height = usize::from(self.rows.saturating_sub(1));
    let mut palette = avt::Vt::new(2, 1);
    palette.feed_str("\x1b[7mX");
    let selected_pen = *palette.line(0).cells()[0].pen();
    for (row, line) in mode.lines.iter().enumerate().skip(mode.top).take(height) {
      let mut column = 0;
      for (index, ch) in line.iter().enumerate() {
        let width = CopyMode::width(*ch);
        if width == 0 {
          continue;
        }
        if column >= mode.left && column + width <= mode.left + usize::from(self.columns) {
          let x = u16::try_from(column - mode.left).expect("bounded column");
          let y = u16::try_from(row - mode.top).expect("bounded row");
          let pen = if mode.selected(Position { row, column: index }) {
            selected_pen
          } else {
            avt::Pen::default()
          };
          self.set(
            x,
            y,
            Pixel {
              ch: *ch,
              width: u8::try_from(width).expect("glyph width"),
              pen,
            },
          );
          if width == 2 {
            self.set(
              x + 1,
              y,
              Pixel {
                ch: ' ',
                width: 0,
                pen,
              },
            );
          }
        }
        column += width;
        if column >= mode.left + usize::from(self.columns) {
          break;
        }
      }
    }
    let x = mode.cursor_column().saturating_sub(mode.left);
    let y = mode.cursor.row.saturating_sub(mode.top);
    if x < usize::from(self.columns) && y < height {
      self.cursor = Some((
        u16::try_from(x).expect("bounded"),
        u16::try_from(y).expect("bounded"),
      ));
    }
  }

  pub fn overlay(&mut self, lines: &[String]) {
    if lines.is_empty() {
      return;
    }
    self.cursor = None;
    for (row, line) in lines
      .iter()
      .take(usize::from(self.rows.saturating_sub(1)))
      .enumerate()
    {
      self.text(0, u16::try_from(row).expect("bounded row"), line, true);
    }
  }
}

fn viewport_offset(
  view: &ViewInfo,
  pane: Option<&Pane>,
  focused: &str,
  width: u16,
  height: u16,
) -> (u16, u16) {
  let Some(rect) = view.panes.iter().find(|rect| rect.terminal_id == focused) else {
    return (0, 0);
  };
  let cursor = pane.map(|pane| pane.model.vt.cursor());
  let x = usize::from(rect.left) + cursor.map_or(0, |cursor| cursor.col);
  let y = usize::from(rect.top) + cursor.map_or(0, |cursor| cursor.row);
  let left = x
    .saturating_sub(usize::from(width.saturating_sub(1)))
    .min(usize::from(view.canvas_size.columns.saturating_sub(width)));
  let top = y
    .saturating_sub(usize::from(height.saturating_sub(1)))
    .min(usize::from(view.canvas_size.rows.saturating_sub(height)));
  (
    u16::try_from(left).expect("bounded canvas"),
    u16::try_from(top).expect("bounded canvas"),
  )
}

#[derive(Default)]
pub struct Renderer {
  previous: Option<Frame>,
}

impl Renderer {
  pub fn invalidate(&mut self) {
    self.previous = None;
  }

  pub fn draw(&mut self, frame: Frame) -> io::Result<()> {
    if self.previous.as_ref() == Some(&frame) {
      return Ok(());
    }
    let mut output = io::stdout().lock();
    self.write(&mut output, &frame)?;
    output.flush()?;
    self.previous = Some(frame);
    Ok(())
  }

  fn write(&self, output: &mut impl Write, frame: &Frame) -> io::Result<()> {
    queue!(output, Hide)?;
    let previous = self
      .previous
      .as_ref()
      .filter(|previous| previous.columns == frame.columns && previous.rows == frame.rows);
    let mut position = None;
    let mut current_pen = None;
    for y in 0..frame.rows {
      for x in 0..frame.columns {
        let index = usize::from(y) * usize::from(frame.columns) + usize::from(x);
        let pixel = frame.cells[index];
        if pixel.width == 0 {
          continue;
        }
        if previous.is_some_and(|previous| previous.cells[index] == pixel) {
          continue;
        }
        if position != Some((x, y)) {
          queue!(output, MoveTo(x, y))?;
        }
        if current_pen != Some(pixel.pen) {
          queue!(output, SetAttribute(Attribute::Reset), ResetColor)?;
          style(output, &pixel.pen)?;
          current_pen = Some(pixel.pen);
        }
        queue!(output, Print(pixel.ch))?;
        position = Some((x.saturating_add(u16::from(pixel.width)), y));
      }
    }
    queue!(output, SetAttribute(Attribute::Reset), ResetColor)?;
    if let Some((x, y)) = frame.cursor {
      queue!(output, MoveTo(x, y), Show)?;
    }
    Ok(())
  }
}

fn color(color: avt::Color) -> Color {
  match color {
    avt::Color::Indexed(index) => Color::AnsiValue(index),
    avt::Color::RGB(rgb) => Color::Rgb {
      r: rgb.r,
      g: rgb.g,
      b: rgb.b,
    },
  }
}

fn style(output: &mut impl Write, pen: &avt::Pen) -> io::Result<()> {
  if let Some(foreground) = pen.foreground() {
    queue!(output, SetForegroundColor(color(foreground)))?;
  }
  if let Some(background) = pen.background() {
    queue!(output, SetBackgroundColor(color(background)))?;
  }
  for (enabled, attribute) in [
    (pen.is_bold(), Attribute::Bold),
    (pen.is_faint(), Attribute::Dim),
    (pen.is_italic(), Attribute::Italic),
    (pen.is_underline(), Attribute::Underlined),
    (pen.is_inverse(), Attribute::Reverse),
    (pen.is_strikethrough(), Attribute::CrossedOut),
    (pen.is_blink(), Attribute::SlowBlink),
  ] {
    if enabled {
      queue!(output, SetAttribute(attribute))?;
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn status_text_cannot_inject_terminal_controls() {
    let mut frame = Frame::new(40, 3);
    frame.text(0, 2, "test\x1b[2J\nname", true);
    assert_eq!(frame.cells[80].ch, 't');
    assert!(
      !frame
        .cells
        .iter()
        .any(|pixel| pixel.ch == '\x1b' || pixel.ch == '\n')
    );
  }

  #[test]
  fn dividers_use_reserved_cells_and_leave_the_status_row_free() {
    use rmux_proto::{SplitAxis, TerminalSize, ViewLayout};
    let layout = ViewLayout::Split {
      axis: SplitAxis::Horizontal,
      children: vec![
        ViewLayout::Terminal {
          terminal_id: "a".into(),
        },
        ViewLayout::Terminal {
          terminal_id: "b".into(),
        },
      ],
    };
    let canvas_size = TerminalSize {
      columns: 100,
      rows: 40,
      pixel_width: 0,
      pixel_height: 0,
    };
    let view = ViewInfo {
      session_id: "s".into(),
      session_name: "test".into(),
      view_id: "v".into(),
      revision: 0,
      panes: layout.pane_geometry(&canvas_size).unwrap(),
      layout,
      canvas_size,
      terminals: Vec::new(),
    };
    let mut frame = Frame::new(100, 41);
    frame.canvas(&view, &BTreeMap::new(), "a");
    assert_eq!(frame.cells[50].ch, '│');
    assert_eq!(
      frame.cells[50].pen.foreground(),
      Some(avt::Color::Indexed(6))
    );
    assert_eq!(frame.cells[49].ch, ' ');
    assert_eq!(frame.cells[51].ch, ' ');
    assert_eq!(frame.cells[3950].ch, '│');
    assert_eq!(frame.cells[4050].ch, ' ');
    frame.text(0, 40, "status", true);
    assert!(frame.cells[4099].pen.is_inverse());
  }

  #[test]
  fn copy_view_clips_wide_glyphs_and_highlights_both_cells() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut mode = CopyMode::new(vec!["a界b".into()]);
    mode.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), 2);
    mode.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), 2);
    let mut frame = Frame::new(4, 3);
    frame.copy_mode(&mode);
    assert_eq!(frame.cells[1].ch, '界');
    assert_eq!(frame.cells[1].width, 2);
    assert!(frame.cells[1].pen.is_inverse());
    assert_eq!(frame.cells[2].width, 0);
    assert!(frame.cells[2].pen.is_inverse());
    mode.left = 2;
    let mut clipped = Frame::new(2, 3);
    clipped.copy_mode(&mode);
    assert_eq!(clipped.cells[0].ch, ' ');
    assert_eq!(clipped.cells[1].ch, 'b');
  }

  #[test]
  fn unchanged_frames_do_not_rewrite_cells() {
    let frame = Frame::new(10, 4);
    let renderer = Renderer {
      previous: Some(Frame::new(10, 4)),
    };
    let mut bytes = Vec::new();
    renderer.write(&mut bytes, &frame).unwrap();
    assert!(!bytes.contains(&b' '));
  }
}
