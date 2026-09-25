use rmux_proto::{TerminalCheckpoint, TerminalSize};

/// A PTY-sized emulator. Host viewport changes never resize this model.
pub struct Model {
  pub vt: avt::Vt,
  pub bracketed_paste: bool,
  history: Vec<String>,
  pending: Vec<u8>,
  escape: String,
  string_control: bool,
  string_escape: bool,
}

impl Model {
  pub fn new(size: &TerminalSize) -> Self {
    Self {
      vt: avt::Vt::builder()
        .size(
          usize::from(size.columns.max(2)),
          usize::from(size.rows.max(1)),
        )
        .scrollback_limit(2_000)
        .build(),
      bracketed_paste: false,
      history: Vec::new(),
      pending: Vec::new(),
      escape: String::new(),
      string_control: false,
      string_escape: false,
    }
  }

  pub fn restore(&mut self, checkpoint: &TerminalCheckpoint) {
    *self = Self::new(&checkpoint.terminal_size);
    // Restores must not answer historical terminal queries.
    self.feed(&checkpoint.payload);
    self.pending.extend_from_slice(&checkpoint.input_prefix);
  }

  pub fn set_history(&mut self, history: Vec<String>) {
    self.history = history;
  }

  pub fn copy_lines(&self) -> Vec<String> {
    let mut lines = self.history.clone();
    lines.extend(self.vt.text());
    lines
  }

  pub fn resize(&mut self, size: &TerminalSize) {
    if self
      .vt
      .resize(usize::from(size.columns), usize::from(size.rows))
      .scrollback
      .next()
      .is_some()
    {
      self.history.clear();
    }
  }

  pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
    self.pending.extend_from_slice(bytes);
    let mut replies = Vec::new();
    loop {
      let (length, invalid) = match std::str::from_utf8(&self.pending) {
        Ok(_) => (self.pending.len(), None),
        Err(error) => (error.valid_up_to(), error.error_len()),
      };
      if length > 0 {
        let text =
          String::from_utf8(self.pending.drain(..length).collect()).expect("validated UTF-8");
        for ch in text.chars() {
          self.vt.feed(ch);
          replies.extend(self.control(ch));
        }
      }
      if let Some(length) = invalid {
        self.pending.drain(..length);
        self.vt.feed('\u{fffd}');
      } else {
        break;
      }
    }
    if self.vt.feed_str("").scrollback.next().is_some() {
      // Once local rows are evicted, the checkpoint's older history is no
      // longer contiguous with this buffer. Keep only the retained suffix.
      self.history.clear();
    }
    replies
  }

  fn control(&mut self, ch: char) -> Vec<u8> {
    if self.string_control {
      if ch == '\x07' || (self.string_escape && ch == '\\') {
        self.string_control = false;
      }
      self.string_escape = ch == '\x1b';
      return Vec::new();
    }
    if ch == '\x1b' {
      self.escape = ch.to_string();
      return Vec::new();
    }
    if self.escape.is_empty() {
      return Vec::new();
    }
    if self.escape == "\x1b" && matches!(ch, ']' | 'P' | '_' | '^') {
      self.escape.clear();
      self.string_control = true;
      return Vec::new();
    }
    self.escape.push(ch);
    if self.escape == "\x1b[" {
      return Vec::new();
    }
    if !self.escape.starts_with("\x1b[") || self.escape.len() > 64 {
      self.escape.clear();
      return Vec::new();
    }
    if !('@'..='~').contains(&ch) {
      return Vec::new();
    }
    let sequence = std::mem::take(&mut self.escape);
    match sequence.as_str() {
      "\x1b[3J" => self.history.clear(),
      "\x1b[?2004h" => self.bracketed_paste = true,
      "\x1b[?2004l" => self.bracketed_paste = false,
      "\x1b[5n" => return b"\x1b[0n".to_vec(),
      "\x1b[6n" => {
        let cursor = self.vt.cursor();
        return format!("\x1b[{};{}R", cursor.row + 1, cursor.col + 1).into_bytes();
      }
      "\x1b[c" | "\x1b[0c" => return b"\x1b[?1;2c".to_vec(),
      "\x1b[>c" | "\x1b[>0c" => return b"\x1b[>0;1;0c".to_vec(),
      _ => {}
    }
    Vec::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  fn model() -> Model {
    Model::new(&TerminalSize {
      columns: 12,
      rows: 3,
      pixel_width: 0,
      pixel_height: 0,
    })
  }

  #[test]
  fn utf8_and_csi_survive_fragmented_output() {
    let mut model = model();
    model.feed(&[0xe7, 0x95]);
    model.feed(&[0x8c, 27, b'[', b'3']);
    model.feed(b"1mX");
    assert!(model.vt.text()[0].starts_with("界X"));
    assert_eq!(
      model.vt.line(0).cells()[2].pen().foreground(),
      Some(avt::Color::Indexed(1))
    );
  }

  #[test]
  fn checkpoint_reset_retains_pending_utf8_and_resets_screen() {
    let mut model = model();
    model.feed(b"old");
    model.restore(&TerminalCheckpoint {
      format: rmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: rmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence: 7,
      terminal_size: TerminalSize {
        columns: 12,
        rows: 3,
        pixel_width: 0,
        pixel_height: 0,
      },
      payload: b"new".to_vec(),
      input_prefix: vec![0xe7, 0x95],
    });
    model.feed(&[0x8c]);
    assert!(model.vt.text()[0].starts_with("new界"));
  }

  #[test]
  fn local_eviction_drops_the_older_checkpoint_prefix() {
    let mut model = model();
    model.set_history(vec!["old-checkpoint".into()]);
    model.feed("row\r\n".repeat(2_100).as_bytes());
    assert!(!model.copy_lines().join("\n").contains("old-checkpoint"));
    assert!(model.copy_lines().len() <= 2_003);
  }

  #[test]
  fn copy_joins_soft_wraps_and_preserves_interior_spaces_and_newlines() {
    let mut model = model();
    model.feed(b"abcd  efghijklmnop  \r\nnext");
    let lines = model.copy_lines();
    assert_eq!(lines[0], "abcd  efghijklmnop");
    assert_eq!(lines[1], "next");
  }

  #[test]
  fn history_survives_live_output_and_checkpoint_replacement_without_duplication() {
    let mut model = model();
    model.set_history(vec!["before-attach".into()]);
    model.feed(b"one\r\ntwo\r\nthree\r\nfour");
    let text = model.copy_lines().join("\n");
    assert!(text.starts_with("before-attach\none\ntwo\nthree\nfour"));
    let frozen = model.copy_lines();
    model.feed(b"\r\nfive");
    assert!(!frozen.join("\n").contains("five"));
    model.restore(&TerminalCheckpoint {
      format: rmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: rmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence: 100,
      terminal_size: TerminalSize {
        columns: 12,
        rows: 3,
        pixel_width: 0,
        pixel_height: 0,
      },
      payload: b"four\r\nfive".to_vec(),
      input_prefix: Vec::new(),
    });
    model.set_history(vec![
      "before-attach".into(),
      "one".into(),
      "two".into(),
      "three".into(),
    ]);
    let text = model.copy_lines().join("\n");
    assert_eq!(text.matches("one").count(), 1);
    assert_eq!(text.matches("four").count(), 1);
    model.feed(b"\x1b[3J");
    assert!(!model.copy_lines().join("\n").contains("before-attach"));
  }

  #[test]
  fn queries_and_bracketed_paste_are_pane_local() {
    let mut model = model();
    assert_eq!(model.feed(b"ab\x1b[6n"), b"\x1b[1;3R");
    model.feed(b"\x1b[?2004h");
    assert!(model.bracketed_paste);
    model.feed(b"\x1b]0;\x1b[?2004l\x07");
    assert!(model.bracketed_paste);
    model.feed(b"\x1b[?2004l");
    assert!(!model.bracketed_paste);
  }
}
