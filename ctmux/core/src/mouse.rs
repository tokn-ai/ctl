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

/// A bounded observer for application mouse and bracketed-paste modes.
///
/// Feed decoded output characters, including incomplete controls. String
/// payloads never change input modes; ESC and C1 controls cancel strings as
/// they do in the screen emulator. C1 controls are supported because the
/// screen emulator uses them when checkpointing its incomplete parser state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TerminalInputModes {
  mouse: MouseModes,
  bracketed_paste: bool,
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

  /// Observe one decoded character from the application's terminal output.
  pub fn feed(&mut self, ch: char) {
    if matches!(ch, '\x18' | '\x1a' | '\u{80}'..='\u{8f}' | '\u{91}'..='\u{97}' | '\u{99}' | '\u{9a}' | '\u{9c}')
    {
      self.control = Control::Ground;
      return;
    }
    match ch {
      '\x1b' => {
        self.control = Control::Escape;
        return;
      }
      '\u{9b}' => {
        self.control = Control::Csi(String::new());
        return;
      }
      '\u{9d}' => {
        self.control = Control::String { osc: true };
        return;
      }
      '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
        self.control = Control::String { osc: false };
        return;
      }
      _ => {}
    }
    if let Control::String { osc } = self.control {
      if osc && ch == '\x07' {
        self.control = Control::Ground;
      }
      return;
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
        }
        ch if ch.is_ascii_control() => self.control = Control::Escape,
        _ => {}
      },
      Control::Csi(mut parameters) => {
        if ('@'..='~').contains(&ch) || ch > '\u{9f}' {
          if matches!(ch, 'h' | 'l') {
            self.private_modes(&parameters, ch == 'h');
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
