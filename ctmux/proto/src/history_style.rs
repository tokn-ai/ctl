//! Bounded presentation attributes for history text, independent of terminal controls.

use crate::TerminalHistoryRow;
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalColor {
  Indexed { index: u8 },
  Rgb { red: u8, green: u8, blue: u8 },
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip predicates borrow the field.
fn is_false(value: &bool) -> bool {
  !value
}

/// Attributes applied to a range of Unicode scalar values in a history row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Independent terminal SGR attributes.
pub struct TerminalStyle {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub foreground: Option<TerminalColor>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub background: Option<TerminalColor>,
  #[serde(skip_serializing_if = "is_false")]
  pub bold: bool,
  #[serde(skip_serializing_if = "is_false")]
  pub faint: bool,
  #[serde(skip_serializing_if = "is_false")]
  pub italic: bool,
  #[serde(skip_serializing_if = "is_false")]
  pub underline: bool,
  #[serde(skip_serializing_if = "is_false")]
  pub strikethrough: bool,
  #[serde(skip_serializing_if = "is_false")]
  pub blink: bool,
  #[serde(skip_serializing_if = "is_false")]
  pub inverse: bool,
}

impl TerminalStyle {
  /// Generate only trusted SGR attributes, resetting the preceding style first.
  #[must_use]
  pub fn sgr(&self) -> String {
    let mut codes = vec!["0".to_owned()];
    for (enabled, code) in [
      (self.bold, "1"),
      (self.faint, "2"),
      (self.italic, "3"),
      (self.underline, "4"),
      (self.blink, "5"),
      (self.inverse, "7"),
      (self.strikethrough, "9"),
    ] {
      if enabled {
        codes.push(code.into());
      }
    }
    for (color, selector) in [(self.foreground, 38), (self.background, 48)] {
      if let Some(color) = color {
        codes.push(match color {
          TerminalColor::Indexed { index } => format!("{selector};5;{index}"),
          TerminalColor::Rgb { red, green, blue } => {
            format!("{selector};2;{red};{green};{blue}")
          }
        });
      }
    }
    format!("\x1b[{}m", codes.join(";"))
  }
}

/// Nonempty, ordered, nonoverlapping character ranges; gaps use default styling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalHistoryStyleSpan {
  pub start: u32,
  pub length: u32,
  pub style: TerminalStyle,
}

/// Validate text and attribute ranges before replaying a history row.
///
/// # Errors
/// Rejects terminal controls and empty, overlapping, or out-of-bounds spans.
pub fn validate_history_row(row: &TerminalHistoryRow) -> io::Result<()> {
  let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
  if row.text.chars().any(char::is_control) {
    return Err(invalid("history row contains terminal control characters"));
  }
  let length = u32::try_from(row.text.chars().count())
    .map_err(|_| invalid("history row exceeds its character bound"))?;
  let mut previous_end = 0;
  for span in &row.styles {
    let Some(end) = span.start.checked_add(span.length) else {
      return Err(invalid("history style range overflows"));
    };
    if span.length == 0 || span.start < previous_end || end > length {
      return Err(invalid(
        "history style range is outside its row or overlaps",
      ));
    }
    previous_end = end;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn row() -> TerminalHistoryRow {
    TerminalHistoryRow {
      text: "界ab".into(),
      wrapped: false,
      styles: vec![TerminalHistoryStyleSpan {
        start: 0,
        length: 1,
        style: TerminalStyle {
          foreground: Some(TerminalColor::Indexed { index: 1 }),
          ..TerminalStyle::default()
        },
      }],
    }
  }

  #[test]
  fn old_rows_remain_byte_identical_and_styles_use_character_offsets() {
    let plain: TerminalHistoryRow =
      serde_json::from_str(r#"{"text":"界ab","wrapped":false}"#).unwrap();
    assert_eq!(plain.styles, Vec::<TerminalHistoryStyleSpan>::new());
    assert_eq!(
      serde_json::to_string(&plain).unwrap(),
      r#"{"text":"界ab","wrapped":false}"#
    );
    let row = row();
    validate_history_row(&row).unwrap();
    assert_eq!(
      serde_json::from_str::<TerminalHistoryRow>(&serde_json::to_string(&row).unwrap()).unwrap(),
      row
    );
  }

  #[test]
  fn malformed_style_ranges_and_controls_are_rejected() {
    for (start, length) in [(0, 0), (3, 1), (1, 3), (u32::MAX, 1)] {
      let mut row = row();
      row.styles[0].start = start;
      row.styles[0].length = length;
      assert!(validate_history_row(&row).is_err());
    }
    let mut overlap = row();
    overlap.styles.push(overlap.styles[0].clone());
    assert!(validate_history_row(&overlap).is_err());
    let mut control = row();
    control.text.push('\x1b');
    assert!(validate_history_row(&control).is_err());
    let mut intensity = row();
    intensity.styles[0].style.bold = true;
    intensity.styles[0].style.faint = true;
    validate_history_row(&intensity).unwrap();
  }

  #[test]
  fn sgr_contains_only_typed_colors_and_attributes() {
    let style = TerminalStyle {
      foreground: Some(TerminalColor::Indexed { index: 255 }),
      background: Some(TerminalColor::Rgb {
        red: 12,
        green: 34,
        blue: 56,
      }),
      bold: true,
      italic: true,
      underline: true,
      strikethrough: true,
      blink: true,
      inverse: true,
      faint: false,
    };
    assert_eq!(style.sgr(), "\x1b[0;1;3;4;5;7;9;38;5;255;48;2;12;34;56m");
    assert_eq!(TerminalStyle::default().sgr(), "\x1b[0m");
    assert!(serde_json::from_str::<TerminalColor>(r#"{"kind":"indexed","index":256}"#).is_err());
  }
}
