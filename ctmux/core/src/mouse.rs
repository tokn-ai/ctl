//! Input modes not retained by the screen emulator's ANSI checkpoints.

/// Which pointer events an application requests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum MouseTracking {
  #[default]
  None,
  Buttons,
  Drag,
  Any,
}

/// The application's requested mouse report representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncoding {
  Legacy,
  Utf8,
  Sgr,
  Urxvt,
  /// Cell coordinates cannot represent this mode without pixel dimensions.
  SgrPixels,
}

/// Mouse tracking and encoding are independent DEC private modes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MouseModes {
  tracking: MouseTracking,
  encodings: u8,
}

const UTF8: u8 = 1;
const SGR: u8 = 2;
const URXVT: u8 = 4;
const SGR_PIXELS: u8 = 8;

/// The xterm modified-character reporting level requested by the application.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ModifyOtherKeys {
  #[default]
  Off,
  Mode1,
  Mode2,
}

impl ModifyOtherKeys {
  #[must_use]
  pub fn level(self) -> u8 {
    match self {
      Self::Off => 0,
      Self::Mode1 => 1,
      Self::Mode2 => 2,
    }
  }
}

impl MouseModes {
  #[must_use]
  pub fn enabled(self) -> bool {
    self.tracking != MouseTracking::None
  }

  #[must_use]
  pub fn tracking(self) -> MouseTracking {
    self.tracking
  }

  #[must_use]
  pub fn encoding(self) -> MouseEncoding {
    if self.encodings & SGR_PIXELS != 0 {
      MouseEncoding::SgrPixels
    } else if self.encodings & SGR != 0 {
      MouseEncoding::Sgr
    } else if self.encodings & URXVT != 0 {
      MouseEncoding::Urxvt
    } else if self.encodings & UTF8 != 0 {
      MouseEncoding::Utf8
    } else {
      MouseEncoding::Legacy
    }
  }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum Control {
  #[default]
  Ground,
  Escape,
  Csi(String),
  IgnoreCsi,
  String {
    osc: bool,
  },
}

/// A bounded observer for application mouse, paste, and keyboard modes.
///
/// Feed decoded output characters, including incomplete controls. String
/// payloads never change input modes; ESC and C1 controls cancel strings as
/// they do in the screen emulator. C1 controls are supported because the
/// screen emulator uses them when checkpointing its incomplete parser state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TerminalInputModes {
  mouse: MouseModes,
  bracketed_paste: bool,
  modify_other_keys: ModifyOtherKeys,
  control: Control,
}

impl TerminalInputModes {
  #[must_use]
  pub fn mouse(&self) -> MouseModes {
    self.mouse
  }

  #[must_use]
  pub fn bracketed_paste(&self) -> bool {
    self.bracketed_paste
  }

  #[must_use]
  pub fn modify_other_keys(&self) -> ModifyOtherKeys {
    self.modify_other_keys
  }

  /// Observe one decoded character from the application's terminal output.
  /// Returns true when a completed control queries the current keyboard level.
  /// Observers that do not own input can ignore that query.
  pub fn feed(&mut self, ch: char) -> bool {
    if matches!(ch, '\x18' | '\x1a' | '\u{80}'..='\u{8f}' | '\u{91}'..='\u{97}' | '\u{99}' | '\u{9a}' | '\u{9c}')
    {
      self.control = Control::Ground;
      return false;
    }
    match ch {
      '\x1b' => {
        self.control = Control::Escape;
        return false;
      }
      '\u{9b}' => {
        self.control = Control::Csi(String::new());
        return false;
      }
      '\u{9d}' => {
        self.control = Control::String { osc: true };
        return false;
      }
      '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
        self.control = Control::String { osc: false };
        return false;
      }
      _ => {}
    }
    if let Control::String { osc } = self.control {
      if osc && ch == '\x07' {
        self.control = Control::Ground;
      }
      return false;
    }
    let control = std::mem::take(&mut self.control);
    match control {
      Control::Escape => match ch {
        '[' => self.control = Control::Csi(String::new()),
        ']' => self.control = Control::String { osc: true },
        'P' | 'X' | '^' | '_' => {
          self.control = Control::String { osc: false };
        }
        'c' => {
          self.mouse = MouseModes::default();
          self.bracketed_paste = false;
          self.modify_other_keys = ModifyOtherKeys::Off;
        }
        ch if ch.is_ascii_control() => self.control = Control::Escape,
        _ => {}
      },
      Control::Csi(mut parameters) => {
        if ('@'..='~').contains(&ch) || ch > '\u{9f}' {
          if ch == 'm' && parameters == "?4" {
            return true;
          }
          if matches!(ch, 'h' | 'l') {
            self.private_modes(&parameters, ch == 'h');
          } else if matches!(ch, 'm' | 'n') {
            self.keyboard_modes(&parameters, ch);
          }
        } else if ch.is_ascii_control() {
          self.control = Control::Csi(parameters);
        } else if (' '..='?').contains(&ch) && parameters.len() < 64 {
          parameters.push(ch);
          self.control = Control::Csi(parameters);
        } else {
          self.control = Control::IgnoreCsi;
        }
      }
      Control::IgnoreCsi if !('@'..='~').contains(&ch) && ch <= '\u{9f}' => {
        self.control = Control::IgnoreCsi;
      }
      _ => {}
    }
    false
  }

  /// Restore these modes on an initially reset terminal before its screen dump.
  ///
  /// Prepending is essential: the emulator's dump can end inside an incomplete
  /// control sequence. Appending mode controls would replace that parser state.
  #[must_use]
  pub fn restore_sequences(&self) -> String {
    let mut modes = Vec::new();
    match self.mouse.tracking {
      MouseTracking::None => {}
      MouseTracking::Buttons => modes.push(1000),
      MouseTracking::Drag => modes.push(1002),
      MouseTracking::Any => modes.push(1003),
    }
    for (mode, bit) in [(1005, UTF8), (1006, SGR), (1015, URXVT), (1016, SGR_PIXELS)] {
      if self.mouse.encodings & bit != 0 {
        modes.push(mode);
      }
    }
    if self.bracketed_paste {
      modes.push(2004);
    }
    if modes.is_empty() {
      String::new()
    } else {
      let parameters = modes
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(";");
      format!("\x1b[?{parameters}h")
    }
  }

  /// Restore keyboard reporting separately so older negotiated contracts can
  /// retain their original checkpoint payload without examining screen bytes.
  #[must_use]
  pub fn restore_keyboard_sequence(&self) -> String {
    format!("\x1b[>4;{}m", self.modify_other_keys.level())
  }

  fn keyboard_modes(&mut self, parameters: &str, final_byte: char) {
    let Some(parameters) = parameters.strip_prefix('>') else {
      return;
    };
    if parameters.is_empty() && final_byte == 'm' {
      self.modify_other_keys = ModifyOtherKeys::Off;
      return;
    }
    let mut values = parameters.split(';');
    if values.next().and_then(|value| value.parse::<u16>().ok()) != Some(4) {
      return;
    }
    let level = values.next();
    if values.next().is_some() {
      return;
    }
    match (final_byte, level) {
      ('m', None | Some("")) | ('n', None) => {
        self.modify_other_keys = ModifyOtherKeys::Off;
      }
      ('m', Some(value)) => match value.parse::<u16>() {
        Ok(0) => self.modify_other_keys = ModifyOtherKeys::Off,
        Ok(1) => self.modify_other_keys = ModifyOtherKeys::Mode1,
        Ok(2) => self.modify_other_keys = ModifyOtherKeys::Mode2,
        _ => {}
      },
      _ => {}
    }
  }

  fn private_modes(&mut self, parameters: &str, enabled: bool) {
    let Some(parameters) = parameters.strip_prefix('?') else {
      return;
    };
    let Ok(modes) = parameters
      .split(';')
      .map(str::parse::<u16>)
      .collect::<Result<Vec<_>, _>>()
    else {
      return;
    };
    for mode in modes {
      match mode {
        1000 | 1002 | 1003 => {
          // tmux and xterm have one active tracking mode. Setting a mode
          // replaces the previous one; resetting any tracking mode stops it.
          self.mouse.tracking = match (enabled, mode) {
            (true, 1000) => MouseTracking::Buttons,
            (true, 1002) => MouseTracking::Drag,
            (true, 1003) => MouseTracking::Any,
            _ => MouseTracking::None,
          };
        }
        1001 if !enabled => self.mouse.tracking = MouseTracking::None,
        1005 => self.encoding(UTF8, enabled),
        1006 => self.encoding(SGR, enabled),
        1015 => self.encoding(URXVT, enabled),
        1016 => self.encoding(SGR_PIXELS, enabled),
        2004 => self.bracketed_paste = enabled,
        _ => {}
      }
    }
  }

  fn encoding(&mut self, encoding: u8, enabled: bool) {
    if enabled {
      self.mouse.encodings |= encoding;
    } else {
      self.mouse.encodings &= !encoding;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn feed(modes: &mut TerminalInputModes, output: &str) {
    for ch in output.chars() {
      modes.feed(ch);
    }
  }

  #[test]
  fn combined_modes_follow_tracking_replacement_and_independent_encoding() {
    let mut modes = TerminalInputModes::default();
    feed(&mut modes, "\x1b[?25;1000;1002;1006;2004h");
    assert_eq!(modes.mouse().tracking(), MouseTracking::Drag);
    assert_eq!(modes.mouse().encoding(), MouseEncoding::Sgr);
    assert!(modes.bracketed_paste());
    feed(&mut modes, "\x1b[?1000l");
    assert!(!modes.mouse().enabled());
    assert_eq!(modes.mouse().encoding(), MouseEncoding::Sgr);
    feed(&mut modes, "\x1b[?1003h\x1b[?1006;2004l");
    assert_eq!(modes.mouse().tracking(), MouseTracking::Any);
    assert_eq!(modes.mouse().encoding(), MouseEncoding::Legacy);
    assert!(!modes.bracketed_paste());
  }

  #[test]
  fn strings_and_cancelled_or_oversized_controls_cannot_set_modes() {
    let mut modes = TerminalInputModes::default();
    for output in [
      "\x1b]0;[?1002;1006h\x07",
      "\x1bPignored\x07[?1002h\x1b\\",
      "\u{9d}0;[?2004h\u{9c}",
      "\x1b[?1002\x18h",
      "\x1b[?1002\x1ah",
      "\x1b[?1002:1h",
    ] {
      feed(&mut modes, output);
    }
    feed(&mut modes, &format!("\x1b[?{};1002h", "0;".repeat(40)));
    assert!(!modes.mouse().enabled());
    assert!(!modes.bracketed_paste());
    feed(&mut modes, "\x1b[?1002h");
    assert!(modes.mouse().enabled());
  }

  #[test]
  fn fragments_c1_parser_prefix_and_terminal_reset_preserve_semantics() {
    let mut modes = TerminalInputModes::default();
    feed(&mut modes, "\x1b[?100");
    assert!(!modes.mouse().enabled());
    feed(&mut modes, "2;1006h");
    let mut restored = TerminalInputModes::default();
    feed(&mut restored, &modes.restore_sequences());
    feed(&mut restored, "\u{9b}?100");
    feed(&mut restored, "3h");
    assert_eq!(restored.mouse().tracking(), MouseTracking::Any);
    assert_eq!(restored.mouse().encoding(), MouseEncoding::Sgr);
    feed(&mut restored, "\x1b[?2004h\x1bc");
    assert_eq!(restored, TerminalInputModes::default());
  }

  #[test]
  fn restoration_round_trips_supported_and_unsupported_encoding_requests() {
    let mut modes = TerminalInputModes::default();
    feed(&mut modes, "\x1b[?1003;1005;1006;1015;1016;2004h");
    let mut restored = TerminalInputModes::default();
    feed(&mut restored, &modes.restore_sequences());
    assert_eq!(restored, modes);
    feed(&mut restored, "\x1b[?1016l");
    assert_eq!(restored.mouse().encoding(), MouseEncoding::Sgr);
    feed(&mut restored, "\x1b[?1006l");
    assert_eq!(restored.mouse().encoding(), MouseEncoding::Urxvt);
    feed(&mut restored, "\x1b[?1015l");
    assert_eq!(restored.mouse().encoding(), MouseEncoding::Utf8);
  }

  #[test]
  fn modified_key_levels_and_reset_controls_follow_xterm_requests() {
    let mut modes = TerminalInputModes::default();
    for (output, expected) in [
      ("\x1b[>4;1m", ModifyOtherKeys::Mode1),
      ("\x1b[>4;2m", ModifyOtherKeys::Mode2),
      ("\x1b[>4;0m", ModifyOtherKeys::Off),
      ("\x1b[>4;2m\x1b[>4m", ModifyOtherKeys::Off),
      ("\x1b[>4;2m\x1b[>4;m", ModifyOtherKeys::Off),
      ("\x1b[>4;2m\x1b[>m", ModifyOtherKeys::Off),
      ("\x1b[>4;2m\x1b[>4n", ModifyOtherKeys::Off),
      ("\x1b[>4;2m\x1bc", ModifyOtherKeys::Off),
    ] {
      feed(&mut modes, output);
      assert_eq!(modes.modify_other_keys(), expected, "{output:?}");
      assert_eq!(expected.level(), modes.modify_other_keys().level());
    }
  }

  #[test]
  fn keyboard_modes_ignore_queries_strings_and_invalid_parameters() {
    let mut modes = TerminalInputModes::default();
    feed(&mut modes, "\x1b[>4;1m");
    for output in [
      "\x1b[?4m",
      "\x1b[4;2m",
      "\x1b[>1;2m",
      "\x1b[>4;3m",
      "\x1b[>4;-1m",
      "\x1b[>4;2;0m",
      "\x1b[>4:2m",
      "\x1b[>4;2n",
      "\x1b[>4;2\x18m",
      "\x1b]title >4;2m\x07",
      "\x1bPdata >4;2m\x1b\\",
      "\u{9d}title >4;2m\u{9c}",
    ] {
      feed(&mut modes, output);
      assert_eq!(
        modes.modify_other_keys(),
        ModifyOtherKeys::Mode1,
        "{output:?}"
      );
    }
    feed(&mut modes, &format!("\x1b[>4;{}2m", "0".repeat(64)));
    assert_eq!(modes.modify_other_keys(), ModifyOtherKeys::Mode1);
  }

  #[test]
  fn keyboard_fragments_and_restore_leave_other_input_modes_independent() {
    let mut modes = TerminalInputModes::default();
    feed(&mut modes, "\x1b[?1002;1006;2004h\x1b[>4;");
    assert_eq!(modes.modify_other_keys(), ModifyOtherKeys::Off);
    feed(&mut modes, "2m");
    let mut restored = TerminalInputModes::default();
    feed(&mut restored, &modes.restore_keyboard_sequence());
    feed(&mut restored, &modes.restore_sequences());
    assert_eq!(restored, modes);
    feed(&mut restored, "\u{9b}>4;");
    feed(&mut restored, "1m");
    assert_eq!(restored.modify_other_keys(), ModifyOtherKeys::Mode1);
    assert_eq!(restored.mouse(), modes.mouse());
    assert!(restored.bracketed_paste());
  }

  #[test]
  fn escape_and_c1_cancel_strings_like_the_checkpoint_screen_parser() {
    let mut modes = TerminalInputModes::default();
    feed(&mut modes, "\x1b]title\x1b[?1002;1006h");
    assert_eq!(modes.mouse().tracking(), MouseTracking::Drag);
    feed(&mut modes, "\x1bPdata\u{9b}?1003h");
    assert_eq!(modes.mouse().tracking(), MouseTracking::Any);
    feed(&mut modes, "\x1b[!p");
    // DECSTR changes the basic DEC terminal modes, not xterm mouse reporting.
    assert_eq!(modes.mouse().tracking(), MouseTracking::Any);
    assert_eq!(modes.mouse().encoding(), MouseEncoding::Sgr);
  }
}
