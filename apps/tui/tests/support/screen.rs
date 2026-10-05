use std::collections::VecDeque;
use std::fmt::Write as _;

const TRANSCRIPT_LIMIT: usize = 64 * 1_024;

/// A frozen view of the terminal's visible buffer, with zero-based coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
  pub rows: Vec<String>,
  pub columns: usize,
  pub cursor: (usize, usize),
  pub cursor_visible: bool,
  pub closed: bool,
  pub error: Option<String>,
}

impl Screen {
  pub fn contains(&self, text: &str) -> bool {
    self.rows.iter().any(|row| row.contains(text))
  }

  pub fn row(&self, index: usize) -> &str {
    self.rows.get(index).map_or("", String::as_str)
  }

  pub fn diagnostic(&self) -> String {
    let mut result = format!(
      "screen {}x{}, cursor {:?} ({}), stream {}\n",
      self.columns,
      self.rows.len(),
      self.cursor,
      if self.cursor_visible {
        "visible"
      } else {
        "hidden"
      },
      if self.closed { "closed" } else { "open" },
    );
    if let Some(error) = &self.error {
      writeln!(result, "capture error: {error}").expect("write to string");
    }
    for (index, row) in self.rows.iter().enumerate() {
      writeln!(result, "{index:03}: {row:?}").expect("write to string");
    }
    result
  }
}

/// Incrementally decodes PTY bytes and retains a bounded transcript for failures.
pub struct Capture {
  vt: avt::Vt,
  pending: Vec<u8>,
  transcript: VecDeque<u8>,
  discarded: usize,
  closed: bool,
  error: Option<String>,
}

impl Capture {
  pub fn new(columns: u16, rows: u16) -> Self {
    Self {
      vt: avt::Vt::builder()
        .size(usize::from(columns.max(1)), usize::from(rows.max(1)))
        .scrollback_limit(0)
        .build(),
      pending: Vec::new(),
      transcript: VecDeque::new(),
      discarded: 0,
      closed: false,
      error: None,
    }
  }

  pub fn feed(&mut self, bytes: &[u8]) {
    if self.closed {
      return;
    }
    self.record(bytes);
    let mut input = std::mem::take(&mut self.pending);
    input.extend_from_slice(bytes);
    let mut offset = 0;
    while offset < input.len() {
      let (valid, invalid) = match std::str::from_utf8(&input[offset..]) {
        Ok(text) => (text.len(), None),
        Err(error) => (error.valid_up_to(), error.error_len()),
      };
      let end = offset + valid;
      self.feed_text(std::str::from_utf8(&input[offset..end]).expect("validated UTF-8"));
      offset = end;
      match invalid {
        Some(length) => {
          self.feed_text("\u{fffd}");
          offset += length;
        }
        None => break,
      }
    }
    self.pending.extend_from_slice(&input[offset..]);
  }

  pub fn resize(&mut self, columns: u16, rows: u16) {
    self
      .vt
      .resize(usize::from(columns.max(1)), usize::from(rows.max(1)))
      .scrollback
      .for_each(drop);
  }

  pub fn snapshot(&self) -> Screen {
    let cursor = self.vt.cursor();
    Screen {
      // Vt::text() is the primary buffer, even while the alternate buffer is active.
      rows: self.vt.view().map(avt::Line::text).collect(),
      columns: self.vt.size().0,
      cursor: (cursor.col, cursor.row),
      cursor_visible: cursor.visible,
      closed: self.closed,
      error: self.error.clone(),
    }
  }

  pub fn diagnostic(&self) -> String {
    let mut result = self.snapshot().diagnostic();
    writeln!(
      result,
      "raw transcript ({} bytes retained, {} discarded):",
      self.transcript.len(),
      self.discarded,
    )
    .expect("write to string");
    for byte in &self.transcript {
      for escaped in byte.escape_ascii() {
        result.push(char::from(escaped));
      }
    }
    result.push('\n');
    result
  }

  pub fn finish(&mut self, error: Option<String>) {
    if !self.pending.is_empty() {
      // A UTF-8 prefix cannot be completed once the stream has ended.
      self.pending.clear();
      self.feed_text("\u{fffd}");
    }
    self.closed = true;
    if self.error.is_none() {
      self.error = error;
    }
  }

  fn record(&mut self, bytes: &[u8]) {
    let overflow = (self.transcript.len() + bytes.len()).saturating_sub(TRANSCRIPT_LIMIT);
    self.discarded = self.discarded.saturating_add(overflow);
    if bytes.len() >= TRANSCRIPT_LIMIT {
      self.transcript.clear();
      self
        .transcript
        .extend(&bytes[bytes.len() - TRANSCRIPT_LIMIT..]);
    } else {
      self.transcript.drain(..overflow);
      self.transcript.extend(bytes);
    }
  }

  fn feed_text(&mut self, text: &str) {
    // AVT releases evicted rows during feed_str. Drain regularly even for a
    // large newline-heavy read, instead of retaining all of its scrollback.
    let batch_limit = (16_384 / self.vt.size().0).clamp(1, 256);
    for (index, ch) in text.chars().enumerate() {
      self.vt.feed(ch);
      if (index + 1) % batch_limit == 0 {
        self.vt.feed_str("").scrollback.for_each(drop);
      }
    }
    self.vt.feed_str("").scrollback.for_each(drop);
  }
}

#[cfg(test)]
mod tests {
  use super::{Capture, TRANSCRIPT_LIMIT};

  #[test]
  fn chunks_preserve_unicode_and_ansi_parser_state() {
    let bytes = "\x1b[2;3H界é\x1b[1;1HX".as_bytes();
    let mut whole = Capture::new(12, 3);
    whole.feed(bytes);
    let expected = whole.snapshot();
    for split in 0..=bytes.len() {
      let mut capture = Capture::new(12, 3);
      capture.feed(&bytes[..split]);
      capture.feed(&bytes[split..]);
      assert_eq!(capture.snapshot(), expected, "split at byte {split}");
    }
    let mut capture = Capture::new(12, 3);
    for byte in bytes {
      capture.feed(&[*byte]);
    }
    assert_eq!(capture.snapshot(), expected);
    assert_eq!(expected.row(0), "X           ");
    assert!(expected.row(1).starts_with("  界é"));
    assert_eq!(expected.cursor, (1, 0));
  }

  #[test]
  fn snapshot_tracks_visible_alternate_screen_and_cursor() {
    let mut capture = Capture::new(12, 3);
    capture.feed(b"primary\x1b[?1049h\x1b[2;1Halt\x1b[?25l");
    let alternate = capture.snapshot();
    assert!(!alternate.contains("primary"));
    assert!(alternate.row(1).starts_with("alt"));
    assert_eq!(alternate.cursor, (3, 1));
    assert!(!alternate.cursor_visible);

    capture.feed(b"\x1b[?1049l\x1b[?25h");
    let primary = capture.snapshot();
    assert!(primary.row(0).starts_with("primary"));
    assert!(!primary.contains("alt"));
    assert_eq!(primary.cursor, (7, 0));
    assert!(primary.cursor_visible);
    // Snapshots remain independent when the active buffer changes.
    assert!(alternate.contains("alt"));
  }

  #[test]
  fn resize_keeps_visible_rows_and_cursor_in_bounds() {
    let mut capture = Capture::new(8, 3);
    capture.feed(b"one\r\ntwo\r\nthree\r\nfour");
    assert_eq!(capture.snapshot().rows.len(), 3);
    assert!(!capture.snapshot().contains("one"));
    capture.resize(6, 2);
    let screen = capture.snapshot();
    assert_eq!(screen.columns, 6);
    assert_eq!(screen.rows.len(), 2);
    assert!(screen.contains("four"));
    assert!(screen.cursor.0 <= screen.columns);
    assert!(screen.cursor.1 < screen.rows.len());
    assert_eq!(screen.row(100), "");
  }

  #[test]
  fn invalid_and_truncated_utf8_are_replaced_at_stream_end() {
    let mut capture = Capture::new(12, 2);
    capture.feed(&[b'A', 0xff, b'B', 0xe7, 0x95]);
    assert!(capture.snapshot().row(0).starts_with("A\u{fffd}B"));
    assert_eq!(capture.snapshot().cursor, (3, 0));
    capture.finish(Some("reader failed".to_owned()));
    capture.finish(None);
    let screen = capture.snapshot();
    assert!(screen.row(0).starts_with("A\u{fffd}B\u{fffd}"));
    assert_eq!(screen.cursor, (4, 0));
    assert!(screen.closed);
    assert_eq!(screen.error.as_deref(), Some("reader failed"));
    assert!(
      capture
        .diagnostic()
        .contains("capture error: reader failed")
    );
  }

  #[test]
  fn transcript_retains_exact_bounded_suffix() {
    let mut capture = Capture::new(8, 2);
    capture.feed(&vec![b'A'; TRANSCRIPT_LIMIT - 2]);
    capture.feed(b"BCDEF");
    assert_eq!(capture.transcript.len(), TRANSCRIPT_LIMIT);
    assert_eq!(capture.discarded, 3);
    assert_eq!(
      capture
        .transcript
        .iter()
        .rev()
        .take(5)
        .copied()
        .collect::<Vec<_>>(),
      b"FEDCB"
    );

    let bytes = vec![b'Z'; TRANSCRIPT_LIMIT + 10];
    capture.feed(&bytes);
    assert_eq!(capture.transcript.len(), TRANSCRIPT_LIMIT);
    assert_eq!(capture.discarded, TRANSCRIPT_LIMIT + 13);
    assert!(capture.transcript.iter().all(|byte| *byte == b'Z'));
    assert!(capture.diagnostic().contains("discarded):"));
  }
}
