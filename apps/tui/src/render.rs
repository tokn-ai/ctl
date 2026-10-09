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
use ctmux_proto::ViewInfo;
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
  #[cfg(test)]
  pub fn text_rows(&self) -> Vec<String> {
    self
      .cells
      .chunks(usize::from(self.columns).max(1))
      .map(|row| {
        row
          .iter()
          .filter(|pixel| pixel.width != 0)
          .map(|pixel| pixel.ch)
          .collect()
      })
      .collect()
  }
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

  pub fn command_prompt(&mut self, prompt: &crate::prompt::Prompt) {
    if self.columns == 0 || self.rows == 0 {
      self.cursor = None;
      return;
    }
    let (text, cursor_column) = prompt.display(self.columns);
    let row = self.rows - 1;
    self.text(0, row, &text, true);
    self.cursor = Some((cursor_column.min(self.columns - 1), row));
  }

  /// Paint local number badges without replacing pane output or the footer.
  pub fn pane_numbers(
    &mut self,
    view: &ViewInfo,
    labels: &[(String, usize)],
    focused: &str,
    offset: (u16, u16),
  ) {
    if labels.is_empty() {
      return;
    }
    self.cursor = None;
    let mut palette = avt::Vt::new(2, 1);
    palette.feed_str("\x1b[1;7;36mX\x1b[0;7mX");
    for pane in view.visible_panes() {
      let Some((_, number)) = labels.iter().find(|(id, _)| id == &pane.terminal_id) else {
        continue;
      };
      let Some((left, top, right, bottom)) =
        badge_bounds(&pane, offset, self.columns, self.rows.saturating_sub(1))
      else {
        continue;
      };
      let number = number.to_string();
      let available = usize::from(right - left);
      // A partial number could select a different pane. Omit labels that do
      // not fit; a one-cell slice can still show an entire single-digit label.
      if number.len() > available {
        continue;
      }
      let badge = if number.len() + 2 <= available {
        format!(" {number} ")
      } else {
        number
      };
      let width = u16::try_from(badge.len()).expect("badge fits visible pane");
      let x = left + (right - left - width) / 2;
      let y = top + (bottom - top - 1) / 2;
      let pen = *palette.line(0).cells()[usize::from(pane.terminal_id != focused)].pen();
      self.clear_badge_glyphs(x, y, width, (left, right));
      for (column, ch) in badge.chars().enumerate() {
        self.set(
          x + u16::try_from(column).expect("bounded badge column"),
          y,
          Pixel { ch, width: 1, pen },
        );
      }
    }
  }

  fn clear_badge_glyphs(&mut self, x: u16, y: u16, width: u16, bounds: (u16, u16)) {
    let row = usize::from(y) * usize::from(self.columns);
    for column in x..x + width {
      let index = row + usize::from(column);
      let neighbor = match self.cells[index].width {
        0 if column > bounds.0 => Some(column - 1),
        2 if column + 1 < bounds.1 => Some(column + 1),
        _ => None,
      };
      if let Some(neighbor) = neighbor {
        let pen = self.cells[row + usize::from(neighbor)].pen;
        self.set(
          neighbor,
          y,
          Pixel {
            ch: ' ',
            width: 1,
            pen,
          },
        );
      }
    }
  }

  pub fn canvas(
    &mut self,
    view: &ViewInfo,
    panes: &BTreeMap<String, Pane>,
    copies: &BTreeMap<String, CopyMode>,
    focused: &str,
  ) {
    let height = self.rows.saturating_sub(1);
    if height == 0 || self.columns == 0 {
      return;
    }
    let (offset_x, offset_y) = viewport_offset(
      view,
      panes.get(focused),
      copies.get(focused),
      focused,
      self.columns,
      height,
    );
    self.canvas_at(view, panes, copies, focused, (offset_x, offset_y));
  }

  pub fn canvas_at(
    &mut self,
    view: &ViewInfo,
    panes: &BTreeMap<String, Pane>,
    copies: &BTreeMap<String, CopyMode>,
    focused: &str,
    (offset_x, offset_y): (u16, u16),
  ) {
    let height = self.rows.saturating_sub(1);
    if height == 0 || self.columns == 0 {
      return;
    }
    // Only cells not covered by a pane become dividers; no border reduces PTY space.
    let mut covered = vec![false; usize::from(self.columns) * usize::from(height)];
    for rect in &view.visible_panes() {
      let pane = panes.get(&rect.terminal_id);
      let copy = copies.get(&rect.terminal_id);
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
          if copy.is_none()
            && let Some(cell) = cells.and_then(|line| line.cells().get(usize::from(column)))
          {
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
      if let Some(mode) = copy {
        let cursor = self.cursor;
        self.copy_region(
          mode,
          (rect.left, rect.top),
          (rect.columns, rect.rows),
          (offset_x, offset_y),
        );
        if rect.terminal_id != focused {
          self.cursor = cursor;
        }
      }
      if copy.is_none()
        && let Some(ended) = pane.and_then(|pane| pane.ended.as_deref())
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
        && copy.is_none()
        && let Some(pane) = pane
      {
        self.live_cursor(pane, rect, (offset_x, offset_y));
      }
    }
    self.dividers(view, &covered, (offset_x, offset_y), focused);
  }

  fn live_cursor(&mut self, pane: &Pane, rect: &ctmux_proto::PaneGeometry, offset: (u16, u16)) {
    let cursor = pane.model.vt.cursor();
    if !cursor.visible
      || !pane.connected
      || cursor.col >= usize::from(rect.columns)
      || cursor.row >= usize::from(rect.rows)
    {
      return;
    }
    let x = (usize::from(rect.left) + cursor.col).checked_sub(usize::from(offset.0));
    let y = (usize::from(rect.top) + cursor.row).checked_sub(usize::from(offset.1));
    if let (Some(x), Some(y)) = (x, y)
      && x < usize::from(self.columns)
      && y < usize::from(self.rows.saturating_sub(1))
    {
      self.cursor = Some((
        u16::try_from(x).expect("bounded"),
        u16::try_from(y).expect("bounded"),
      ));
    }
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
    self.copy_region(
      mode,
      (0, 0),
      (self.columns, self.rows.saturating_sub(1)),
      (0, 0),
    );
  }

  fn copy_region(
    &mut self,
    mode: &CopyMode,
    origin: (u16, u16),
    size: (u16, u16),
    offset: (u16, u16),
  ) {
    let height = usize::from(size.1);
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
        if column >= mode.left && column + width <= mode.left + usize::from(size.0) {
          let absolute_x = usize::from(origin.0) + column - mode.left;
          let absolute_y = usize::from(origin.1) + row - mode.top;
          let Some(x) = absolute_x
            .checked_sub(usize::from(offset.0))
            .filter(|x| x + width <= usize::from(self.columns))
          else {
            column += width;
            continue;
          };
          let Some(y) = absolute_y
            .checked_sub(usize::from(offset.1))
            .filter(|y| *y < usize::from(self.rows.saturating_sub(1)))
          else {
            column += width;
            continue;
          };
          let x = u16::try_from(x).expect("bounded column");
          let y = u16::try_from(y).expect("bounded row");
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
        if column >= mode.left + usize::from(size.0) {
          break;
        }
      }
    }
    let x = mode.cursor_column().saturating_sub(mode.left);
    let y = mode.cursor.row.saturating_sub(mode.top);
    let x = (usize::from(origin.0) + x).checked_sub(usize::from(offset.0));
    let y = (usize::from(origin.1) + y).checked_sub(usize::from(offset.1));
    if let (Some(x), Some(y)) = (x, y)
      && x < usize::from(self.columns)
      && y < usize::from(self.rows.saturating_sub(1))
    {
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

fn badge_bounds(
  pane: &ctmux_proto::PaneGeometry,
  offset: (u16, u16),
  columns: u16,
  rows: u16,
) -> Option<(u16, u16, u16, u16)> {
  let (x, y) = (u32::from(offset.0), u32::from(offset.1));
  let left = u32::from(pane.left).saturating_sub(x);
  let top = u32::from(pane.top).saturating_sub(y);
  let right = (u32::from(pane.left) + u32::from(pane.columns))
    .saturating_sub(x)
    .min(u32::from(columns));
  let bottom = (u32::from(pane.top) + u32::from(pane.rows))
    .saturating_sub(y)
    .min(u32::from(rows));
  (left < right && top < bottom).then(|| {
    (
      u16::try_from(left).expect("visible left edge"),
      u16::try_from(top).expect("visible top edge"),
      u16::try_from(right).expect("visible right edge"),
      u16::try_from(bottom).expect("visible bottom edge"),
    )
  })
}

pub fn pane_at<'a>(
  view: &'a ViewInfo,
  panes: &BTreeMap<String, Pane>,
  copies: &BTreeMap<String, CopyMode>,
  focused: &str,
  size: (u16, u16),
  position: (u16, u16),
) -> Option<&'a str> {
  let height = size.1.saturating_sub(1);
  if position.0 >= size.0 || position.1 >= height {
    return None;
  }
  let offset = viewport_offset(
    view,
    panes.get(focused),
    copies.get(focused),
    focused,
    size.0,
    height,
  );
  let x = position.0.checked_add(offset.0)?;
  let y = position.1.checked_add(offset.1)?;
  if let Some(zoomed) = view.zoomed_terminal_id.as_deref() {
    return (x < view.canvas_size.columns && y < view.canvas_size.rows).then_some(zoomed);
  }
  view
    .panes
    .iter()
    .find(|pane| {
      x >= pane.left && x - pane.left < pane.columns && y >= pane.top && y - pane.top < pane.rows
    })
    .map(|pane| pane.terminal_id.as_str())
}

/// Map host coordinates into a pane, clamping a captured drag to its boundaries.
pub fn pane_position(
  view: &ViewInfo,
  panes: &BTreeMap<String, Pane>,
  copies: &BTreeMap<String, CopyMode>,
  focused: &str,
  size: (u16, u16),
  target: &str,
  position: (u16, u16),
) -> Option<(u16, u16)> {
  let rect = view
    .visible_panes()
    .into_iter()
    .find(|rect| rect.terminal_id == target)?;
  let offset = viewport_offset(
    view,
    panes.get(focused),
    copies.get(focused),
    focused,
    size.0,
    size.1.saturating_sub(1),
  );
  Some((
    position
      .0
      .saturating_add(offset.0)
      .saturating_sub(rect.left)
      .min(rect.columns.saturating_sub(1)),
    position
      .1
      .saturating_add(offset.1)
      .saturating_sub(rect.top)
      .min(rect.rows.saturating_sub(1)),
  ))
}

pub(crate) fn viewport_offset(
  view: &ViewInfo,
  pane: Option<&Pane>,
  copy: Option<&CopyMode>,
  focused: &str,
  width: u16,
  height: u16,
) -> (u16, u16) {
  let Some(rect) = view
    .visible_panes()
    .into_iter()
    .find(|rect| rect.terminal_id == focused)
  else {
    return (0, 0);
  };
  let cursor = pane.map(|pane| pane.model.vt.cursor());
  let x = usize::from(rect.left)
    + copy.map_or_else(
      || cursor.map_or(0, |cursor| cursor.col),
      |copy| copy.cursor_column().saturating_sub(copy.left),
    );
  let y = usize::from(rect.top)
    + copy.map_or_else(
      || cursor.map_or(0, |cursor| cursor.row),
      |copy| copy.cursor.row.saturating_sub(copy.top),
    );
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

  fn number_view(columns: u16, rows: u16) -> ViewInfo {
    use ctmux_proto::{SplitAxis, TerminalSize, ViewLayout};
    let canvas_size = TerminalSize {
      columns,
      rows,
      ..TerminalSize::default()
    };
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
      weights: Vec::new(),
    };
    ViewInfo {
      session_name: "numbers".into(),
      session_id: "session".into(),
      view_id: "view".into(),
      revision: 0,
      panes: layout.pane_geometry(&canvas_size).unwrap(),
      canvas_size,
      zoomed_terminal_id: None,
      layout,
      terminals: Vec::new(),
    }
  }

  #[test]
  fn pane_numbers_paint_only_compact_badges_with_distinct_focused_style() {
    let view = number_view(20, 4);
    let mut frame = Frame::new(20, 5);
    for row in 0..4 {
      frame.text(0, row, "abcdefghijklmnopqrst", false);
    }
    frame.text(0, 4, "connected | history ready", true);
    frame.cursor = Some((0, 0));
    let before = frame.cells.clone();
    frame.pane_numbers(&view, &[("a".into(), 1), ("b".into(), 12)], "a", (0, 0));
    assert_eq!(frame.cursor, None);
    assert_eq!(frame.text_rows()[1], "abc 1 ghijklm 12 rst");
    assert!(frame.cells[24].pen.is_bold());
    assert!(frame.cells[24].pen.is_inverse());
    assert_eq!(
      frame.cells[24].pen.foreground(),
      Some(avt::Color::Indexed(6))
    );
    assert!(!frame.cells[34].pen.is_bold());
    assert!(frame.cells[34].pen.is_inverse());
    for (index, pixel) in frame.cells.iter().enumerate() {
      if !(23..26).contains(&index) && !(33..37).contains(&index) {
        assert!(*pixel == before[index], "unrelated cell {index}");
      }
    }
  }

  #[test]
  fn pane_numbers_clip_to_visible_intersections_and_never_paint_the_footer() {
    let mut view = number_view(20, 4);
    let labels = [("a".into(), 1), ("b".into(), 12)];
    let mut frame = Frame::new(5, 3);
    frame.text(0, 2, "saved", true);
    let footer = frame.cells[10..].to_vec();
    // Only the rightmost cell of a and three cells of b are visible.
    frame.pane_numbers(&view, &labels, "b", (9, 2));
    assert_eq!(frame.text_rows()[0], "1 12 ");
    assert!(frame.cells[1].ch == ' ' && !frame.cells[1].pen.is_inverse());
    assert!(frame.cells[3].pen.is_bold());
    assert!(frame.cells[10..] == footer);
    for (columns, rows, offset) in [
      (0, 0, (0, 0)),
      (0, 3, (0, 0)),
      (1, 1, (0, 0)),
      (4, 3, (30, 30)),
    ] {
      let mut empty = Frame::new(columns, rows);
      let before = empty.cells.clone();
      empty.pane_numbers(&view, &labels, "a", offset);
      assert!(empty.cells == before);
      assert_eq!(empty.cursor, None);
    }
    // A two-digit label in a one-cell slice must not look like pane one.
    let mut narrow = Frame::new(1, 2);
    narrow.pane_numbers(&view, &labels, "b", (19, 0));
    assert_eq!(narrow.cells[0].ch, ' ');
    view.panes[1].left = u16::MAX;
    view.panes[1].columns = u16::MAX;
    narrow.pane_numbers(&view, &labels, "b", (0, 0));
    assert_eq!(narrow.cells[0].ch, '1');
  }

  #[test]
  fn pane_numbers_obey_shared_zoom_and_ignore_hidden_or_unknown_labels() {
    let mut view = number_view(20, 4);
    view.zoomed_terminal_id = Some("b".into());
    let mut frame = Frame::new(20, 5);
    frame.pane_numbers(
      &view,
      &[("a".into(), 1), ("b".into(), 2), ("unknown".into(), 99)],
      "b",
      (0, 0),
    );
    assert_eq!(frame.text_rows()[1].trim(), "2");
    assert!(
      frame
        .text_rows()
        .iter()
        .all(|row| !row.contains('1') && !row.contains('9'))
    );
    let before = frame.cells.clone();
    frame.cursor = Some((3, 0));
    frame.pane_numbers(&view, &[], "b", (0, 0));
    assert!(frame.cells == before);
    assert_eq!(frame.cursor, Some((3, 0)));
  }

  #[test]
  fn pane_number_badges_clear_both_halves_of_intersected_wide_glyphs_and_restore_output() {
    let view = number_view(11, 1);
    let mut before = Frame::new(11, 2);
    before.text(0, 0, "界a界│b界cd", false);
    before.text(0, 1, "status", true);
    let mut host = avt::Vt::new(11, 2);
    let mut bytes = Vec::new();
    Renderer::default().write(&mut bytes, &before).unwrap();
    host.feed_str(std::str::from_utf8(&bytes).unwrap());
    let original = host.text();
    let mut after = Frame {
      columns: before.columns,
      rows: before.rows,
      cells: before.cells.clone(),
      cursor: before.cursor,
    };
    // a's padded badge overlaps the trailing half at its left edge and the
    // leading half at its right edge. b is not labelled and stays unchanged.
    after.pane_numbers(&view, &[("a".into(), 1)], "a", (0, 0));
    assert!(after.cells[..5].iter().all(|pixel| pixel.width == 1));
    assert_eq!(after.text_rows()[0], "  1  │b界cd");
    assert!(after.cells[5..] == before.cells[5..]);
    bytes.clear();
    Renderer {
      previous: Some(before),
    }
    .write(&mut bytes, &after)
    .unwrap();
    host.feed_str(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(host.text()[0], "  1  │b界cd");
    let mut restored = Frame::new(11, 2);
    restored.text(0, 0, "界a界│b界cd", false);
    restored.text(0, 1, "status", true);
    bytes.clear();
    Renderer {
      previous: Some(after),
    }
    .write(&mut bytes, &restored)
    .unwrap();
    host.feed_str(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(host.text(), original);
  }

  #[test]
  fn command_prompt_replaces_only_status_and_moves_the_host_cursor_to_the_footer() {
    let mut before = Frame::new(24, 4);
    before.text(0, 0, "saved output", false);
    before.text(0, 1, "$ echo ready", false);
    before.text(0, 2, "ready", false);
    before.text(0, 3, "connected / history ready", true);
    before.cursor = Some((5, 2));
    let content = before.text_rows()[..3].to_vec();
    let mut bytes = Vec::new();
    Renderer::default().write(&mut bytes, &before).unwrap();
    let mut host = avt::Vt::new(24, 4);
    host.feed_str(std::str::from_utf8(&bytes).unwrap());

    let mut frame = Frame {
      columns: before.columns,
      rows: before.rows,
      cells: before.cells.clone(),
      cursor: before.cursor,
    };
    let mut prompt = crate::prompt::Prompt::default();
    prompt.open();
    prompt.paste("display-panes");
    frame.command_prompt(&prompt);

    assert_eq!(frame.text_rows()[..3], content);
    assert!(frame.text_rows()[3].contains("display-panes"));
    assert!(!frame.text_rows()[3].contains("connected"));
    assert!(frame.cells[72..].iter().all(|pixel| pixel.pen.is_inverse()));
    let cursor_column = prompt.display(24).1;
    assert_eq!(frame.cursor, Some((cursor_column, 3)));

    bytes.clear();
    Renderer {
      previous: Some(before),
    }
    .write(&mut bytes, &frame)
    .unwrap();
    host.feed_str(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(host.cursor().col, usize::from(cursor_column));
    assert_eq!(host.cursor().row, 3);
    assert!(host.cursor().visible);
    let rows: Vec<_> = host.view().map(avt::Line::text).collect();
    assert_eq!(rows[..3], content);
    assert!(rows[3].contains("display-panes"));

    let previous = Frame {
      columns: frame.columns,
      rows: frame.rows,
      cells: frame.cells.clone(),
      cursor: frame.cursor,
    };
    let _ = prompt.key(crossterm::event::KeyEvent::new(
      crossterm::event::KeyCode::Left,
      crossterm::event::KeyModifiers::NONE,
    ));
    frame.command_prompt(&prompt);
    assert_eq!(frame.text_rows(), previous.text_rows());
    assert_eq!(frame.cursor, Some((cursor_column - 1, 3)));
    bytes.clear();
    Renderer {
      previous: Some(previous),
    }
    .write(&mut bytes, &frame)
    .unwrap();
    host.feed_str(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(host.cursor().col, usize::from(cursor_column - 1));
    assert_eq!(host.cursor().row, 3);
  }

  #[test]
  fn command_prompt_handles_tiny_hosts_and_unicode_without_an_extra_terminal_row() {
    let mut prompt = crate::prompt::Prompt::default();
    prompt.open();
    prompt.paste("界界界abcdefgh");
    for (columns, rows) in [(0, 0), (0, 3), (3, 0), (1, 1), (3, 1), (5, 2)] {
      let mut frame = Frame::new(columns, rows);
      frame.text(0, 0, "PTY", false);
      let content = frame.text_rows();
      frame.command_prompt(&prompt);
      assert_eq!(frame.cells.len(), usize::from(columns) * usize::from(rows));
      if columns == 0 || rows == 0 {
        assert_eq!(frame.cursor, None);
        continue;
      }
      let (column, row) = frame.cursor.expect("prompt has a host cursor");
      assert!(column < columns);
      assert_eq!(row, rows - 1);
      let footer = &frame.cells[usize::from(row) * usize::from(columns)..];
      assert!(footer.iter().all(|pixel| pixel.pen.is_inverse()));
      if rows > 1 {
        assert_eq!(frame.text_rows()[0], content[0]);
      }
      let mut bytes = Vec::new();
      Renderer::default().write(&mut bytes, &frame).unwrap();
      let mut host = avt::Vt::new(usize::from(columns), usize::from(rows));
      host.feed_str(std::str::from_utf8(&bytes).unwrap());
      assert_eq!(host.cursor().row, usize::from(rows - 1));
      assert_eq!(host.cursor().col, usize::from(column));
      assert!(host.cursor().visible);
    }
  }

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
    use ctmux_proto::{SplitAxis, TerminalSize, ViewLayout};
    let layout = ViewLayout::Split {
      weights: Vec::new(),
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
      zoomed_terminal_id: None,
      panes: layout.pane_geometry(&canvas_size).unwrap(),
      layout,
      canvas_size,
      terminals: Vec::new(),
    };
    let mut frame = Frame::new(100, 41);
    frame.canvas(&view, &BTreeMap::new(), &BTreeMap::new(), "a");
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
    let panes = BTreeMap::new();
    assert_eq!(
      pane_at(&view, &panes, &BTreeMap::new(), "a", (100, 41), (49, 0)),
      Some("a")
    );
    assert_eq!(
      pane_at(&view, &panes, &BTreeMap::new(), "a", (100, 41), (50, 0)),
      None
    );
    assert_eq!(
      pane_at(&view, &panes, &BTreeMap::new(), "a", (100, 41), (51, 0)),
      Some("b")
    );
    assert_eq!(
      pane_at(&view, &panes, &BTreeMap::new(), "a", (100, 41), (51, 40)),
      None
    );
    assert_eq!(
      pane_at(&view, &panes, &BTreeMap::new(), "b", (10, 5), (9, 0)),
      Some("b")
    );
  }

  #[test]
  fn copy_view_clips_wide_glyphs_and_highlights_both_cells() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut mode = CopyMode::new(vec!["a界b".into()]);
    mode.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), 2);
    mode.key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), 2);
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
  fn pane_copies_keep_dividers_footer_and_focused_cursor_and_share_hit_test_geometry() {
    use ctmux_proto::{SplitAxis, TerminalSize, ViewLayout};
    let layout = ViewLayout::Split {
      weights: Vec::new(),
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
      columns: 20,
      rows: 4,
      ..TerminalSize::default()
    };
    let view = ViewInfo {
      session_id: "s".into(),
      session_name: "test".into(),
      view_id: "v".into(),
      revision: 0,
      zoomed_terminal_id: None,
      panes: layout.pane_geometry(&canvas_size).unwrap(),
      layout,
      canvas_size,
      terminals: Vec::new(),
    };
    let mut mode = CopyMode::new_wrapped((0..30).map(|row| format!("row{row}")).collect(), 10);
    mode.fit(10, 4);
    let copies = BTreeMap::from([
      ("a".into(), mode),
      ("b".into(), CopyMode::new(vec!["OTHER".into()])),
    ]);
    let panes = BTreeMap::new();
    let mut frame = Frame::new(20, 5);
    frame.canvas(&view, &panes, &copies, "a");
    assert_eq!(frame.cursor, Some((0, 3)));
    let rows = frame.text_rows();
    assert!(rows[0].starts_with("row26"));
    assert!(rows[0].contains("│OTHER"));
    assert_eq!(rows[4].trim(), "");
    let mut clipped = Frame::new(5, 3);
    clipped.canvas(&view, &panes, &copies, "a");
    assert_eq!(clipped.cursor, Some((0, 1)));
    assert!(clipped.text_rows()[0].starts_with("row28"));
    assert_eq!(
      pane_position(&view, &panes, &copies, "a", (5, 3), "a", (0, 0)),
      Some((0, 2))
    );
    assert_eq!(pane_at(&view, &panes, &copies, "a", (5, 3), (0, 2)), None);
    assert_eq!(
      pane_position(&view, &panes, &copies, "a", (20, 5), "a", (19, 4)),
      Some((9, 3))
    );
  }

  #[test]
  fn status_updates_leave_scrolled_copy_content_and_cursor_in_place() {
    let mut mode = CopyMode::new((0..20).map(|row| format!("line-{row}")).collect());
    mode.fit(40, 3);
    assert!(mode.top > 0);
    let mut before = Frame::new(40, 4);
    before.copy_mode(&mode);
    before.text(0, 3, " connected | COPY", true);

    let mut host = avt::Vt::new(40, 4);
    let mut output = Vec::new();
    Renderer::default().write(&mut output, &before).unwrap();
    host.feed_str(std::str::from_utf8(&output).unwrap());
    let content = host.text()[..3].to_vec();
    let cursor = host.cursor();

    let mut after = Frame::new(40, 4);
    after.copy_mode(&mode);
    after.text(
      0,
      3,
      " reconnecting | COPY | History incomplete | long help",
      true,
    );
    let renderer = Renderer {
      previous: Some(before),
    };
    output.clear();
    renderer.write(&mut output, &after).unwrap();
    host.feed_str(std::str::from_utf8(&output).unwrap());
    assert_eq!(&host.text()[..3], content);
    assert_eq!(host.cursor(), cursor);
    assert!(host.text()[3].starts_with(" reconnecting"));
  }

  #[test]
  fn zoom_hit_testing_excludes_hidden_panes_and_the_status_row() {
    use ctmux_proto::{SplitAxis, TerminalSize, ViewLayout};
    let layout = ViewLayout::Split {
      weights: Vec::new(),
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
      columns: 20,
      rows: 4,
      ..TerminalSize::default()
    };
    let view = ViewInfo {
      session_id: "s".into(),
      session_name: "test".into(),
      view_id: "v".into(),
      revision: 1,
      zoomed_terminal_id: Some("b".into()),
      panes: layout.pane_geometry(&canvas_size).unwrap(),
      layout,
      canvas_size,
      terminals: Vec::new(),
    };
    let panes = BTreeMap::new();
    let copies = BTreeMap::new();
    // The old divider and the hidden pane's old area now belong to b.
    for position in [(0, 0), (10, 0), (19, 3)] {
      assert_eq!(
        pane_at(&view, &panes, &copies, "b", (20, 5), position),
        Some("b")
      );
    }
    assert_eq!(pane_at(&view, &panes, &copies, "b", (20, 5), (0, 4)), None);
    assert_eq!(
      pane_position(&view, &panes, &copies, "b", (20, 5), "a", (0, 0)),
      None
    );
    assert_eq!(
      pane_position(&view, &panes, &copies, "b", (20, 5), "b", (19, 3)),
      Some((19, 3))
    );
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
