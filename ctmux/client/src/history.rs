//! Reconstruct a bounded authoritative history projection independently of its live screen.
use ctmux_proto::{TerminalCheckpoint, TerminalHistoryRow};
use std::io;

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Split feeds on UTF-8 boundaries so AVT collects bounded scrollback regularly.
/// A byte bound alone is insufficient: each newline can allocate a full-width row.
pub fn projection_chunks(text: &str, columns: usize) -> impl Iterator<Item = &str> {
  const MAX_ADDED_CELLS: usize = 16 * 1024;
  let chunk_bytes = (MAX_ADDED_CELLS / columns.max(1)).clamp(1, 4096);
  let mut remaining = text;
  std::iter::from_fn(move || {
    if remaining.is_empty() {
      return None;
    }
    let mut end = remaining.len().min(chunk_bytes);
    while !remaining.is_char_boundary(end) {
      end -= 1;
    }
    if end == 0 {
      end = remaining.chars().next()?.len_utf8();
    }
    let (chunk, next) = remaining.split_at(end);
    remaining = next;
    Some(chunk)
  })
}

/// Restore physical primary scrollback and the checkpoint's live terminal state.
///
/// # Errors
/// Rejects unsupported checkpoints, invalid UTF-8, control characters in rows,
/// or scrollback limits that cannot be represented on this platform.
pub fn restore_projection(
  checkpoint: &TerminalCheckpoint,
  rows: &[TerminalHistoryRow],
  scrollback_limit: u64,
) -> io::Result<avt::Vt> {
  if !checkpoint.is_supported()
    || checkpoint.terminal_size.columns == 0
    || checkpoint.terminal_size.rows == 0
  {
    return Err(invalid("Unsupported history checkpoint"));
  }
  let payload = std::str::from_utf8(&checkpoint.payload).map_err(io::Error::other)?;
  let size = &checkpoint.terminal_size;
  let dimensions = (usize::from(size.columns), usize::from(size.rows));
  let mut primary = avt::terminal::Terminal::new(dimensions, Some(0));
  let mut parser = avt::parser::Parser::default();
  for character in payload.chars().chain("\x1b[?47l".chars()) {
    if let Some(function) = parser.feed(character) {
      primary.execute(function);
    }
  }
  let mut unwrapper = avt::util::TextUnwrapper::new();
  let live_rows: Vec<_> = primary
    .view()
    .map(|line| TerminalHistoryRow {
      text: line.text(),
      wrapped: unwrapper.push(line).is_none(),
    })
    .collect();
  let mut vt = avt::Vt::builder()
    .size(dimensions.0, dimensions.1)
    .scrollback_limit(usize::try_from(scrollback_limit).map_err(io::Error::other)?)
    .build();
  let mut seed = String::new();
  for (index, row) in rows.iter().chain(&live_rows).enumerate() {
    if row.text.chars().any(char::is_control) {
      return Err(invalid("History rows contain terminal controls"));
    }
    seed.push_str(&row.text);
    if !row.wrapped && index + 1 < rows.len() + live_rows.len() {
      seed.push_str("\r\n");
    }
  }
  seed.push_str("\x1b[H");
  for text in [&seed, payload] {
    for chunk in projection_chunks(text, dimensions.0) {
      drop(vt.feed_str(chunk));
    }
  }
  Ok(vt)
}

/// Apply raw bytes with the same partial UTF-8 handling as the daemon.
///
/// # Errors
/// Returns an error only if a supposedly valid UTF-8 prefix cannot be decoded.
pub fn feed_projection(
  vt: &mut avt::Vt,
  pending_utf8: &mut Vec<u8>,
  data: &[u8],
) -> io::Result<()> {
  feed_projection_with_evictions(vt, pending_utf8, data).map(|_| ())
}

/// Apply raw output and report whether the bounded primary history evicted rows.
///
/// # Errors
/// Returns an error only if a supposedly valid UTF-8 prefix cannot be decoded.
pub fn feed_projection_with_evictions(
  vt: &mut avt::Vt,
  pending_utf8: &mut Vec<u8>,
  data: &[u8],
) -> io::Result<bool> {
  let mut evicted = false;
  pending_utf8.extend_from_slice(data);
  loop {
    let (text, consumed) = match std::str::from_utf8(pending_utf8) {
      Ok(valid) => (valid.to_owned(), valid.len()),
      Err(error) if error.valid_up_to() > 0 => {
        let length = error.valid_up_to();
        (
          String::from_utf8(pending_utf8[..length].to_vec()).map_err(io::Error::other)?,
          length,
        )
      }
      Err(error) => match error.error_len() {
        Some(length) => ("\u{fffd}".into(), length),
        None => break,
      },
    };
    if consumed == 0 {
      break;
    }
    for chunk in projection_chunks(&text, vt.size().0) {
      evicted |= vt.feed_str(chunk).scrollback.count() > 0;
    }
    pending_utf8.drain(..consumed);
  }
  Ok(evicted)
}

/// Consume a projection and return its retained physical primary scrollback.
#[must_use]
pub fn projection_rows(mut vt: avt::Vt) -> Vec<TerminalHistoryRow> {
  // Cancel a partial control before selecting the primary buffer for inspection.
  drop(vt.feed_str("\x18\x1b[?47l"));
  let count = vt.lines().count().saturating_sub(vt.size().1);
  let mut unwrapper = avt::util::TextUnwrapper::new();
  vt.lines()
    .take(count)
    .map(|line| TerminalHistoryRow {
      text: line.text(),
      wrapped: unwrapper.push(line).is_none(),
    })
    .collect()
}

/// The retained start of a logical line whose ending is still in the live grid.
#[must_use]
pub fn wrapped_history_prefix(rows: &[TerminalHistoryRow]) -> String {
  let mut prefix = String::new();
  for row in rows {
    if row.wrapped {
      prefix.push_str(&row.text);
    } else {
      prefix.clear();
    }
  }
  prefix
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctmux_proto::TerminalSize;

  fn checkpoint(vt: &avt::Vt, size: &TerminalSize) -> TerminalCheckpoint {
    TerminalCheckpoint {
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence: 0,
      terminal_size: size.clone(),
      payload: vt.dump().into_bytes(),
      input_prefix: Vec::new(),
    }
  }

  fn history_rows(vt: &avt::Vt, rows: u16) -> Vec<TerminalHistoryRow> {
    let count = vt.lines().count().saturating_sub(usize::from(rows));
    let mut unwrapper = avt::util::TextUnwrapper::new();
    vt.lines()
      .take(count)
      .map(|line| TerminalHistoryRow {
        text: line.text(),
        wrapped: unwrapper.push(line).is_none(),
      })
      .collect()
  }

  #[test]
  fn wide_newline_replay_collects_rows_with_a_cell_bound_and_keeps_utf8_intact() -> io::Result<()> {
    let text = format!("{}界", "\r\n".repeat(4096));
    for columns in [1000, usize::from(u16::MAX)] {
      let chunks: Vec<_> = projection_chunks(&text, columns).collect();
      assert_eq!(chunks.concat(), text);
      assert!(
        chunks
          .iter()
          .all(|chunk| chunk.chars().count() * columns <= (16 * 1024).max(columns))
      );
    }
    let mut vt = avt::Vt::builder()
      .size(1000, 2)
      .scrollback_limit(16)
      .build();
    assert!(feed_projection_with_evictions(
      &mut vt,
      &mut Vec::new(),
      text.as_bytes()
    )?);
    assert_eq!(vt.lines().count(), 18);
    assert_eq!(vt.view().last().unwrap().text().trim_end(), "界");
    Ok(())
  }

  #[test]
  fn physical_history_replay_matches_source_before_and_after_new_output() -> io::Result<()> {
    let size = TerminalSize {
      columns: 4,
      rows: 2,
      ..TerminalSize::default()
    };
    for initial in [
      "one\r\ntwo\r\nend",
      "same\r\nsame\r\nsame",
      "abcdefghijklmnopq",
      "界界a界界bcdef\r\nnext",
      "a\r\n\r\n\r\nend",
      "prefix\r\n\x1b[31mred\x1b[0m\r\nlive",
    ] {
      let mut source = avt::Vt::builder().size(4, 2).scrollback_limit(4).build();
      drop(source.feed_str(initial));
      let rows = history_rows(&source, size.rows);
      let mut restored = restore_projection(&checkpoint(&source, &size), &rows, 4)?;
      assert_eq!(restored.text(), source.text(), "initial {initial:?}");
      assert_eq!(restored.dump(), source.dump(), "screen {initial:?}");
      for output in ["Z", "\r\nnext", "\r\n\r\nlast"] {
        drop(source.feed_str(output));
        feed_projection(&mut restored, &mut Vec::new(), output.as_bytes())?;
        assert_eq!(
          restored.text(),
          source.text(),
          "after {output:?} for {initial:?}"
        );
        assert_eq!(restored.dump(), source.dump(), "screen after {output:?}");
        assert_eq!(
          history_rows(&restored, size.rows),
          history_rows(&source, size.rows)
        );
      }
    }
    Ok(())
  }

  #[test]
  fn alternate_screen_retains_primary_wrapped_history_and_parser_prefix() -> io::Result<()> {
    let size = TerminalSize {
      columns: 3,
      rows: 2,
      ..TerminalSize::default()
    };
    let mut source = avt::Vt::builder().size(3, 2).scrollback_limit(4).build();
    drop(source.feed_str("abcdefghijkl"));
    let rows = history_rows(&source, size.rows);
    drop(source.feed_str("\x1b[?1049h\x1b[HUI\x1b["));
    let mut restored = restore_projection(&checkpoint(&source, &size), &rows, 4)?;
    assert_eq!(restored.text(), source.text());
    assert_eq!(restored.dump(), source.dump());
    drop(source.feed_str("2J\x1b[?1049l\r\nend"));
    feed_projection(&mut restored, &mut Vec::new(), b"2J\x1b[?1049l\r\nend")?;
    assert_eq!(restored.text(), source.text());
    assert_eq!(restored.dump(), source.dump());
    Ok(())
  }

  #[test]
  fn split_utf8_checkpoint_prefix_is_completed_once() -> io::Result<()> {
    let size = TerminalSize {
      columns: 3,
      rows: 2,
      ..TerminalSize::default()
    };
    let mut source = avt::Vt::builder().size(3, 2).scrollback_limit(4).build();
    drop(source.feed_str("abcdef"));
    let mut checkpoint = checkpoint(&source, &size);
    checkpoint.input_prefix = vec![0xc3];
    let mut restored = restore_projection(&checkpoint, &history_rows(&source, size.rows), 4)?;
    let mut pending = checkpoint.input_prefix.clone();
    feed_projection(&mut restored, &mut pending, &[0xa9])?;
    drop(source.feed_str("é"));
    assert_eq!(pending, Vec::<u8>::new());
    assert_eq!(restored.text(), source.text());
    assert_eq!(restored.dump(), source.dump());
    Ok(())
  }
}
