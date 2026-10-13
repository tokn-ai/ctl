//! Preserve cell attributes in literal history rows without storing terminal commands.
use ctmux_proto::{
  TerminalColor, TerminalHistoryRow, TerminalHistoryStyleSpan, TerminalStyle, validate_history_row,
};
use std::io;

fn color(value: avt::Color) -> TerminalColor {
  match value {
    avt::Color::Indexed(index) => TerminalColor::Indexed { index },
    avt::Color::RGB(rgb) => TerminalColor::Rgb {
      red: rgb.r,
      green: rgb.g,
      blue: rgb.b,
    },
  }
}

fn style(pen: &avt::Pen) -> TerminalStyle {
  TerminalStyle {
    foreground: pen.foreground().map(color),
    background: pen.background().map(color),
    bold: pen.is_bold(),
    faint: pen.is_faint(),
    italic: pen.is_italic(),
    underline: pen.is_underline(),
    strikethrough: pen.is_strikethrough(),
    blink: pen.is_blink(),
    inverse: pen.is_inverse(),
  }
}

/// Capture every visible cell, including styled spaces and excluding wide tails.
/// Style offsets count Unicode scalar values in the resulting literal text.
///
/// # Panics
/// Panics if a row exceeds `u32::MAX` characters; protocol dimensions are u16.
#[must_use]
pub fn capture_row(line: &avt::Line, wrapped: bool) -> TerminalHistoryRow {
  let mut row = TerminalHistoryRow {
    text: String::new(),
    wrapped,
    styles: Vec::new(),
  };
  for (offset, cell) in line
    .cells()
    .iter()
    .filter(|cell| cell.width() > 0)
    .enumerate()
  {
    row.text.push(cell.char());
    if cell.pen().is_default() {
      continue;
    }
    // Terminal dimensions are u16 and each occupied cell adds at most one char.
    let start = u32::try_from(offset).expect("terminal row fits style offsets");
    let cell_style = style(cell.pen());
    if let Some(previous) = row.styles.last_mut()
      && previous.start + previous.length == start
      && previous.style == cell_style
    {
      previous.length += 1;
    } else {
      row.styles.push(TerminalHistoryStyleSpan {
        start,
        length: 1,
        style: cell_style,
      });
    }
  }
  row
}

/// Rebuild a row using only validated literal text and generated SGR controls.
/// Each styled span resets afterwards so attributes never leak into other rows.
///
/// # Errors
/// Rejects controls in literal text and invalid or overlapping style ranges.
pub fn row_ansi(row: &TerminalHistoryRow) -> io::Result<String> {
  validate_history_row(row)?;
  if row.styles.is_empty() {
    return Ok(row.text.clone());
  }
  let mut characters = row.text.chars();
  let mut position = 0;
  let mut ansi = String::from("\x1b[0m");
  for span in &row.styles {
    ansi.extend(characters.by_ref().take((span.start - position) as usize));
    ansi.push_str(&span.style.sgr());
    ansi.extend(characters.by_ref().take(span.length as usize));
    ansi.push_str("\x1b[0m");
    position = span.start + span.length;
  }
  ansi.extend(characters);
  Ok(ansi)
}

/// Convert typed attributes without allocating an emulator for every style span.
#[must_use]
pub fn style_pen(style: &TerminalStyle) -> avt::Pen {
  let color = |color| match color {
    TerminalColor::Indexed { index } => avt::Color::Indexed(index),
    TerminalColor::Rgb { red, green, blue } => avt::Color::rgb(red, green, blue),
  };
  let mut pen = avt::Pen::default();
  pen.set_foreground(style.foreground.map(color));
  pen.set_background(style.background.map(color));
  if style.bold {
    pen.set_bold();
  }
  // AVT represents one intensity; match SGR 1;2 when both are present.
  if style.faint {
    pen.set_faint();
  }
  if style.italic {
    pen.set_italic();
  }
  if style.underline {
    pen.set_underline();
  }
  if style.strikethrough {
    pen.set_strikethrough();
  }
  if style.blink {
    pen.set_blink();
  }
  if style.inverse {
    pen.set_inverse();
  }
  pen
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn direct_pen_conversion_matches_all_sgr_attribute_combinations() {
    for flags in 0_u8..128 {
      let style = TerminalStyle {
        foreground: Some(TerminalColor::Rgb {
          red: 12,
          green: 34,
          blue: 56,
        }),
        background: Some(TerminalColor::Indexed { index: 123 }),
        bold: flags & 1 != 0,
        faint: flags & 2 != 0,
        italic: flags & 4 != 0,
        underline: flags & 8 != 0,
        strikethrough: flags & 16 != 0,
        blink: flags & 32 != 0,
        inverse: flags & 64 != 0,
      };
      let mut vt = avt::Vt::new(1, 1);
      drop(vt.feed_str(&format!("{}x", style.sgr())));
      assert_eq!(style_pen(&style), *vt.line(0).cells()[0].pen());
    }
    assert_eq!(style_pen(&TerminalStyle::default()), avt::Pen::default());
  }

  #[test]
  fn row_roundtrip_preserves_wide_color_attributes_and_erased_backgrounds() -> io::Result<()> {
    let mut original = avt::Vt::builder().size(12, 1).build();
    drop(
      original.feed_str("\x1b[1;3;4;5;7;9;38;2;17;34;51;48;5;123mé界\x1b[0mX\x1b[2;31;44m\x1b[K"),
    );
    let source = original.view().next().unwrap();
    let row = capture_row(source, false);
    assert_eq!(row.styles[0].length, 2);
    assert_eq!(row.styles[1].start, 3);
    assert_eq!(row.styles[1].length, 8);
    assert!(row.styles[1].style.faint);
    assert_eq!(style_pen(&row.styles[0].style), *source.cells()[0].pen());
    let mut restored = avt::Vt::builder().size(12, 1).build();
    drop(restored.feed_str(&row_ansi(&row)?));
    assert_eq!(restored.view().next().unwrap().cells(), source.cells());
    Ok(())
  }

  #[test]
  fn default_cells_between_spans_and_following_rows_do_not_inherit_color() -> io::Result<()> {
    let mut vt = avt::Vt::builder().size(4, 2).build();
    drop(vt.feed_str("\x1b[31mA\x1b[0mB\x1b[32mC\x1b[0m"));
    let row = capture_row(vt.view().next().unwrap(), false);
    let mut restored = avt::Vt::builder().size(4, 2).build();
    drop(restored.feed_str(&format!("{}\r\nnext", row_ansi(&row)?)));
    assert_eq!(
      restored.view().next().unwrap().cells(),
      vt.view().next().unwrap().cells()
    );
    assert!(
      restored
        .view()
        .nth(1)
        .unwrap()
        .cells()
        .iter()
        .all(|cell| cell.pen().is_default())
    );
    Ok(())
  }
}
