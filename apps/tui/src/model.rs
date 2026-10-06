use ctmux_core::mouse::TerminalInputModes;
use ctmux_proto::{TerminalCheckpoint, TerminalHistoryRow, TerminalSize};

#[derive(Clone, Copy, PartialEq, Eq)]
enum StringControl {
  None,
  Active,
  Escape,
}

/// A PTY-sized emulator. Host viewport changes never resize this model.
pub struct Model {
  pub vt: avt::Vt,
  pub bracketed_paste: bool,
  pub history_gap: bool,
  pub input_modes: TerminalInputModes,
  buffer_parser: avt::parser::Parser,
  buffer_state: avt::terminal::Terminal,
  history: Vec<String>,
  pending: Vec<u8>,
  escape: String,
  string_control: StringControl,
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
      history_gap: false,
      input_modes: TerminalInputModes::default(),
      buffer_parser: avt::parser::Parser::default(),
      buffer_state: avt::terminal::Terminal::new((2, 1), Some(0)),
      history: Vec::new(),
      pending: Vec::new(),
      escape: String::new(),
      string_control: StringControl::None,
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

  pub fn adopt_history_projection(
    &mut self,
    vt: avt::Vt,
    pending: Vec<u8>,
    history_gap: bool,
  ) -> bool {
    if vt.dump() != self.vt.dump() || pending != self.pending {
      return false;
    }
    self.vt = vt;
    self.pending = pending;
    self.history.clear();
    self.history_gap = history_gap;
    true
  }

  pub fn copy_lines(&self) -> Vec<String> {
    let mut lines = self.history.clone();
    lines.extend(self.vt.text());
    lines
  }

  /// Freeze the active buffer without reflowing its physical screen rows.
  ///
  /// Legacy history is logical text preceding the primary buffer. It must
  /// never appear behind the independent alternate screen of a full-screen app.
  pub fn copy_snapshot(&self) -> (Vec<String>, Vec<TerminalHistoryRow>) {
    let prefix = if self.buffer_state.active_buffer_type() == avt::terminal::BufferType::Primary {
      self.history.clone()
    } else {
      Vec::new()
    };
    let mut unwrapper = avt::util::TextUnwrapper::new();
    let rows = self
      .vt
      .lines()
      .map(|line| {
        let wrapped = unwrapper.push(line).is_none();
        let text = line.text();
        TerminalHistoryRow {
          text: if wrapped {
            text
          } else {
            text.trim_end_matches(' ').to_owned()
          },
          wrapped,
        }
      })
      .collect();
    (prefix, rows)
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
      self.history_gap = true;
    }
  }

  pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
    self.pending.extend_from_slice(bytes);
    let mut replies = Vec::new();
    // AVT collects evicted rows only during feed_str. Collect incrementally
    // so a coalesced newline-heavy frame cannot allocate an unbounded grid.
    let batch_limit = (16_384 / self.vt.size().0.max(1)).clamp(1, 256);
    let mut batch = 0;
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
          self.observe_buffer(ch);
          self.input_modes.feed(ch);
          self.bracketed_paste = self.input_modes.bracketed_paste();
          replies.extend(self.control(ch));
          batch += 1;
          if batch >= batch_limit {
            self.collect_evictions();
            batch = 0;
          }
        }
      }
      if let Some(length) = invalid {
        self.pending.drain(..length);
        self.vt.feed('\u{fffd}');
        self.observe_buffer('\u{fffd}');
        self.input_modes.feed('\u{fffd}');
        self.bracketed_paste = self.input_modes.bracketed_paste();
      } else {
        break;
      }
    }
    self.collect_evictions();
    replies
  }

  fn collect_evictions(&mut self) {
    if self.vt.feed_str("").scrollback.next().is_some() {
      // Once local rows are evicted, the checkpoint's older history is no
      // longer contiguous with this buffer. Keep only the retained suffix.
      self.history.clear();
      self.history_gap = true;
    }
  }

  fn observe_buffer(&mut self, ch: char) {
    use avt::parser::{EdScope, Function};
    match self.buffer_parser.feed(ch) {
      Some(function @ (Function::Decset(_) | Function::Decrst(_))) => {
        self.buffer_state.execute(function);
      }
      Some(function @ Function::Ris) => {
        self.buffer_state.execute(function);
        self.history.clear();
      }
      Some(Function::Ed(EdScope::SavedLines)) => self.history.clear(),
      _ => {}
    }
  }

  fn control(&mut self, ch: char) -> Vec<u8> {
    if self.string_control != StringControl::None {
      self.string_control =
        if ch == '\x07' || (self.string_control == StringControl::Escape && ch == '\\') {
          StringControl::None
        } else if ch == '\x1b' {
          StringControl::Escape
        } else {
          StringControl::Active
        };
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
      self.string_control = StringControl::Active;
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
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
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
  fn copy_snapshot_preserves_physical_wraps_screen_padding_and_cursor_rows() {
    let mut model = Model::new(&TerminalSize {
      columns: 4,
      rows: 3,
      ..TerminalSize::default()
    });
    model.set_history(vec!["older logical history".into()]);
    model.feed(b"ab  c\r\n");
    let (prefix, rows) = model.copy_snapshot();
    assert_eq!(prefix, ["older logical history"]);
    assert_eq!(rows.len(), 3);
    assert_eq!(
      rows[0],
      TerminalHistoryRow {
        text: "ab  ".into(),
        wrapped: true
      }
    );
    assert_eq!(
      rows[1],
      TerminalHistoryRow {
        text: "c".into(),
        wrapped: false
      }
    );
    assert_eq!(
      rows[2],
      TerminalHistoryRow {
        text: String::new(),
        wrapped: false
      }
    );
    assert_eq!(model.vt.cursor().row, 2);
  }

  #[test]
  fn alternate_copy_uses_active_screen_and_omits_primary_history() {
    let mut model = model();
    model.set_history(vec!["saved primary history".into()]);
    model.feed(b"primary\x1b[?1049h\x1b[2;1Halternate");
    let (prefix, rows) = model.copy_snapshot();
    assert_eq!(prefix, Vec::<String>::new());
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].text, "");
    assert_eq!(rows[1].text, "alternate");
    assert_eq!(rows[2].text, "");
    assert!(model.copy_lines().join("\n").contains("primary"));
    model.feed(b"\x1b[?1049l");
    let (prefix, rows) = model.copy_snapshot();
    assert_eq!(prefix, ["saved primary history"]);
    assert_eq!(rows[0].text, "primary");
  }

  #[test]
  fn buffer_tracking_handles_aliases_combined_modes_fragments_and_reset() {
    for mode in [47, 1047, 1049] {
      let mut model = model();
      model.set_history(vec!["primary prefix".into()]);
      model.feed(format!("\x1b[?25;{mode}").as_bytes());
      assert_eq!(model.copy_snapshot().0, ["primary prefix"]);
      model.feed(b"h");
      assert_eq!(model.copy_snapshot().0, Vec::<String>::new());
      model.feed(b"\x1bc");
      model.set_history(vec!["new primary prefix".into()]);
      assert_eq!(model.copy_snapshot().0, ["new primary prefix"]);
    }
  }

  #[test]
  fn checkpoint_restoration_keeps_the_active_alternate_copy_screen() {
    let mut source = model();
    source.feed(b"primary\x1b[?1049h\x1b[3;1Hbottom");
    let mut restored = model();
    restored.restore(&TerminalCheckpoint {
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence: 1,
      terminal_size: TerminalSize {
        columns: 12,
        rows: 3,
        ..TerminalSize::default()
      },
      payload: source.vt.dump().into_bytes(),
      input_prefix: Vec::new(),
    });
    restored.set_history(vec!["primary history".into()]);
    let (prefix, rows) = restored.copy_snapshot();
    assert_eq!(prefix, Vec::<String>::new());
    assert_eq!(rows[2].text, "bottom");
    assert_eq!(restored.vt.cursor().row, 2);
    restored.feed(b"\x1b[?1049l");
    assert_eq!(restored.copy_snapshot().0, ["primary history"]);
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
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
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
    model.feed(b"\x1b]0;[?2004l\x07");
    assert!(model.bracketed_paste);
    model.feed(b"\x1b[?2004l");
    assert!(!model.bracketed_paste);
  }

  #[test]
  fn input_modes_follow_combined_fragmented_controls_and_checkpoint_restore() {
    use ctmux_core::mouse::{MouseEncoding, MouseTracking};
    let mut model = model();
    model.feed(b"\x1b[?1002;100");
    model.feed(b"6;2004h");
    assert_eq!(model.input_modes.mouse().tracking(), MouseTracking::Drag);
    assert_eq!(model.input_modes.mouse().encoding(), MouseEncoding::Sgr);
    assert!(model.bracketed_paste);
    let mut payload = model.input_modes.restore_sequences();
    payload.push_str(&model.vt.dump());
    model.restore(&TerminalCheckpoint {
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence: 1,
      terminal_size: TerminalSize {
        columns: 12,
        rows: 3,
        pixel_width: 0,
        pixel_height: 0,
      },
      payload: payload.into_bytes(),
      input_prefix: Vec::new(),
    });
    assert_eq!(model.input_modes.mouse().tracking(), MouseTracking::Drag);
    assert!(model.bracketed_paste);
    model.feed(b"\x1bc");
    assert!(!model.input_modes.mouse().enabled());
    assert!(!model.bracketed_paste);
  }

  #[test]
  fn checkpoint_parser_prefix_completes_mouse_modes_after_restore() {
    use ctmux_core::mouse::MouseEncoding;
    for prefix in ["\x1b[?100", "\x1b]title\x1b[?100", "\x1bPdata\u{9b}?100"] {
      let mut source = model();
      source.feed(b"\x1b[?1002h");
      source.feed(prefix.as_bytes());
      let mut payload = source.input_modes.restore_sequences();
      payload.push_str(&source.vt.dump());
      let mut restored = model();
      restored.restore(&TerminalCheckpoint {
        format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
        format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
        sequence: 1,
        terminal_size: TerminalSize {
          columns: 12,
          rows: 3,
          pixel_width: 0,
          pixel_height: 0,
        },
        payload: payload.into_bytes(),
        input_prefix: Vec::new(),
      });
      assert_eq!(
        restored.input_modes.mouse().encoding(),
        MouseEncoding::Legacy
      );
      source.feed(b"6h");
      restored.feed(b"6h");
      assert_eq!(restored.input_modes.mouse().encoding(), MouseEncoding::Sgr);
      assert_eq!(restored.input_modes.mouse(), source.input_modes.mouse());
      assert_eq!(restored.vt.dump(), source.vt.dump());
    }
  }

  #[test]
  fn invalid_utf8_cancels_an_incomplete_mouse_control_like_the_screen_parser() {
    let mut model = model();
    model.feed(b"\x1b[?100\xff2h");
    assert!(!model.input_modes.mouse().enabled());
    assert!(model.vt.text()[0].starts_with("2h"));
  }

  #[test]
  fn history_publication_keeps_the_current_screen_and_rejects_stale_work() {
    let size = TerminalSize {
      columns: 12,
      rows: 3,
      ..TerminalSize::default()
    };
    let mut source = avt::Vt::builder().size(12, 3).scrollback_limit(10).build();
    drop(source.feed_str("old\r\none\r\ntwo\r\ncurrent"));
    let checkpoint = TerminalCheckpoint {
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence: 0,
      terminal_size: size,
      payload: source.dump().into_bytes(),
      input_prefix: Vec::new(),
    };
    let rows = [ctmux_proto::TerminalHistoryRow {
      text: "old".into(),
      wrapped: false,
    }];
    let mut model = Model::new(&checkpoint.terminal_size);
    model.restore(&checkpoint);
    model.feed(b"-new");
    let stale = ctmux_client::history::restore_projection(&checkpoint, &rows, 10).unwrap();
    let current_dump = model.vt.dump();
    assert!(!model.adopt_history_projection(stale, Vec::new(), false));
    assert_eq!(model.vt.dump(), current_dump);
    let mut caught_up = ctmux_client::history::restore_projection(&checkpoint, &rows, 10).unwrap();
    drop(caught_up.feed_str("-new"));
    assert!(model.adopt_history_projection(caught_up, Vec::new(), true));
    assert_eq!(model.vt.dump(), current_dump);
    assert!(model.copy_lines().join("\n").starts_with("old\none"));
    assert!(model.history_gap);
  }
}
