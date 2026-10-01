use comfy_table::{ContentArrangement, Table, presets::NOTHING};

/// Borderless human output with terminal-aware wrapping. Redirected output
/// keeps full-width cells; callers retain their separate JSON output paths.
pub fn format<const COLUMNS: usize>(
  headers: [&str; COLUMNS],
  rows: impl IntoIterator<Item = [String; COLUMNS]>,
) -> String {
  let mut table = build(headers, rows);
  table.set_content_arrangement(ContentArrangement::Dynamic);
  table.trim_fmt()
}

fn build<const COLUMNS: usize>(
  headers: [&str; COLUMNS],
  rows: impl IntoIterator<Item = [String; COLUMNS]>,
) -> Table {
  let mut table = Table::new();
  table.load_style(NOTHING).set_header(headers.map(text));
  for row in rows {
    table.add_row(row.map(|value| text(&value)));
  }
  for (index, column) in table.column_iter_mut().enumerate() {
    column.set_padding((0, if index + 1 == COLUMNS { 0 } else { 2 }));
  }
  table
}

/// Keep untrusted values readable without allowing terminal controls or bidi
/// formatting to alter the output. Escaping occurs before width measurement.
pub fn text(value: &str) -> String {
  let mut rendered = String::with_capacity(value.len());
  for character in value.chars() {
    if character.is_control()
      || matches!(character, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    {
      rendered.extend(character.escape_default());
    } else {
      rendered.push(character);
    }
  }
  rendered
}

#[cfg(test)]
mod tests {
  use super::*;
  use unicode_width::UnicodeWidthStr as _;

  #[test]
  fn terminal_wrapping_preserves_long_values_and_escapes_control_sequences() {
    let long = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let mut table = build(["VALUE"], [[long.into()], ["用户\n\u{1b}[2J".into()]]);
    table
      .force_no_tty()
      .set_width(32)
      .set_content_arrangement(ContentArrangement::Dynamic);
    let rendered = table.trim_fmt();
    assert!(rendered.lines().count() > 2);
    assert!(rendered.lines().all(|line| line.width() <= 32));
    assert!(!rendered.contains('\u{1b}'));
    assert!(rendered.replace(char::is_whitespace, "").contains(long));
  }
}
