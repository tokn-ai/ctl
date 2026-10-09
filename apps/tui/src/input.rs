use crossterm::event::{
  KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ctmux_core::mouse::{ModifyOtherKeys, MouseEncoding, MouseModes, MouseTracking};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
  pub key: KeyEvent,
  pub label: String,
}

pub fn parse_prefix(value: &str) -> Result<Prefix, String> {
  let (modifier, key) = value
    .split_once('+')
    .ok_or("use Ctrl+letter or Alt+letter")?;
  let mut chars = key.chars();
  let ch = chars
    .next()
    .filter(char::is_ascii_alphabetic)
    .ok_or("prefix must be a letter")?;
  if chars.next().is_some() {
    return Err("prefix must be one letter".into());
  }
  let modifiers = match modifier.to_ascii_lowercase().as_str() {
    "ctrl" => KeyModifiers::CONTROL,
    "alt" => KeyModifiers::ALT,
    _ => return Err("use Ctrl+letter or Alt+letter".into()),
  };
  Ok(Prefix {
    key: KeyEvent::new(KeyCode::Char(ch.to_ascii_lowercase()), modifiers),
    label: value.to_owned(),
  })
}

pub fn matches_prefix(key: KeyEvent, prefix: &Prefix) -> bool {
  key.code == prefix.key.code && key.modifiers == prefix.key.modifiers
}

/// Encode xterm input using the focused application's requested keyboard mode.
pub fn encode(key: KeyEvent, application_cursor: bool, mode: ModifyOtherKeys) -> Vec<u8> {
  let supported = KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL;
  if key.kind == KeyEventKind::Release || !(key.modifiers - supported).is_empty() {
    // An unsupported modifier must not silently invoke an ordinary shortcut.
    return Vec::new();
  }
  if let Some(data) = encode_other_key(key, mode) {
    return data;
  }
  encode_xterm(key, application_cursor)
}

fn encode_other_key(key: KeyEvent, mode: ModifyOtherKeys) -> Option<Vec<u8>> {
  if mode == ModifyOtherKeys::Off {
    return None;
  }
  let mut modifiers = key.modifiers;
  let codepoint = match key.code {
    KeyCode::Char(ch) => u32::from(ch),
    KeyCode::Enter => 13,
    KeyCode::Tab => 9,
    KeyCode::BackTab => {
      modifiers.insert(KeyModifiers::SHIFT);
      9
    }
    KeyCode::Backspace => 127,
    KeyCode::Esc => 27,
    _ => return None,
  };
  // Printable Shift-only input is already text. Crossterm can synthesize SHIFT
  // from uppercase UTF-8, including Caps Lock and composed input.
  if modifiers.is_empty()
    || (modifiers == KeyModifiers::SHIFT && matches!(key.code, KeyCode::Char(ch) if ch != ' '))
    || (mode == ModifyOtherKeys::Mode1 && legacy_mode1(key, modifiers))
  {
    return None;
  }
  Some(format!("\x1b[27;{};{codepoint}~", modifier_parameter(modifiers)).into_bytes())
}

fn legacy_mode1(key: KeyEvent, modifiers: KeyModifiers) -> bool {
  if modifiers.intersects(KeyModifiers::ALT) && !modifiers.contains(KeyModifiers::CONTROL) {
    return true;
  }
  // User mode preserves typing and well-known control combinations, while
  // program mode reports every modified ordinary key in the extended form.
  if modifiers == KeyModifiers::SHIFT {
    return matches!(key.code, KeyCode::BackTab)
      || matches!(key.code, KeyCode::Char(ch) if ch != ' ');
  }
  modifiers.contains(KeyModifiers::CONTROL)
    && matches!(key.code, KeyCode::Char(' ' | '/' | '2'..='8' | '@'..='~'))
}

fn modifier_parameter(modifiers: KeyModifiers) -> u8 {
  1 + u8::from(modifiers.contains(KeyModifiers::SHIFT))
    + 2 * u8::from(modifiers.contains(KeyModifiers::ALT))
    + 4 * u8::from(modifiers.contains(KeyModifiers::CONTROL))
}

/// Conventional xterm input, including application cursor mode.
fn encode_xterm(key: KeyEvent, application_cursor: bool) -> Vec<u8> {
  let modifiers = key.modifiers;
  let modified =
    modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
  let parameter = modifier_parameter(modifiers);
  let cursor = match key.code {
    KeyCode::Up => Some('A'),
    KeyCode::Down => Some('B'),
    KeyCode::Right => Some('C'),
    KeyCode::Left => Some('D'),
    KeyCode::Home => Some('H'),
    KeyCode::End => Some('F'),
    _ => None,
  };
  if let Some(code) = cursor {
    return if modified {
      format!("\x1b[1;{parameter}{code}").into_bytes()
    } else if application_cursor {
      format!("\x1bO{code}").into_bytes()
    } else {
      format!("\x1b[{code}").into_bytes()
    };
  }
  let tilde = match key.code {
    KeyCode::Insert => Some(2),
    KeyCode::Delete => Some(3),
    KeyCode::PageUp => Some(5),
    KeyCode::PageDown => Some(6),
    KeyCode::F(5) => Some(15),
    KeyCode::F(6) => Some(17),
    KeyCode::F(7) => Some(18),
    KeyCode::F(8) => Some(19),
    KeyCode::F(9) => Some(20),
    KeyCode::F(10) => Some(21),
    KeyCode::F(11) => Some(23),
    KeyCode::F(12) => Some(24),
    _ => None,
  };
  if let Some(code) = tilde {
    return if modified {
      format!("\x1b[{code};{parameter}~").into_bytes()
    } else {
      format!("\x1b[{code}~").into_bytes()
    };
  }
  if let KeyCode::F(number @ 1..=4) = key.code {
    let code = char::from(b'P' + number - 1);
    return if modified {
      format!("\x1b[1;{parameter}{code}").into_bytes()
    } else {
      format!("\x1bO{code}").into_bytes()
    };
  }
  let mut data = match key.code {
    KeyCode::Char(ch) if modifiers.contains(KeyModifiers::CONTROL) => {
      let ch = ch.to_ascii_uppercase();
      match ch {
        '@'..='~' => vec![u8::try_from(u32::from(ch)).expect("ASCII") & 0x1f],
        ' ' | '2' => vec![0],
        '3'..='7' => vec![u8::try_from(u32::from(ch)).expect("ASCII") - b'3' + 27],
        '/' => vec![31],
        '8' | '?' => vec![127],
        _ => Vec::new(),
      }
    }
    KeyCode::Char(ch) => ch.to_string().into_bytes(),
    KeyCode::Enter => vec![b'\r'],
    KeyCode::Tab => vec![b'\t'],
    KeyCode::BackTab => b"\x1b[Z".to_vec(),
    KeyCode::Backspace => vec![127],
    KeyCode::Esc => vec![27],
    _ => Vec::new(),
  };
  if modifiers.contains(KeyModifiers::ALT) && !data.is_empty() {
    data.insert(0, 27);
  }
  data
}

/// Encode an application mouse event with zero-based, pane-local coordinates.
///
/// Tracking determines which events are sent, independently of the requested
/// wire representation. Pixel reports need pixel dimensions unavailable here.
pub fn encode_mouse(event: MouseEvent, modes: MouseModes) -> Option<Vec<u8>> {
  let tracking = modes.tracking();
  if tracking == MouseTracking::None || modes.encoding() == MouseEncoding::SgrPixels {
    return None;
  }
  let release = matches!(event.kind, MouseEventKind::Up(_));
  let button = |button| match button {
    MouseButton::Left => 0,
    MouseButton::Middle => 1,
    MouseButton::Right => 2,
  };
  let mut code: u8 = match event.kind {
    MouseEventKind::Down(value) | MouseEventKind::Up(value) => button(value),
    MouseEventKind::Drag(value) if matches!(tracking, MouseTracking::Drag | MouseTracking::Any) => {
      32 + button(value)
    }
    MouseEventKind::Moved if tracking == MouseTracking::Any => 35,
    MouseEventKind::ScrollUp => 64,
    MouseEventKind::ScrollDown => 65,
    MouseEventKind::ScrollLeft => 66,
    MouseEventKind::ScrollRight => 67,
    _ => return None,
  };
  if release && modes.encoding() != MouseEncoding::Sgr {
    // Legacy protocols identify a release without specifying its button.
    code = 3;
  }
  code += 4 * u8::from(event.modifiers.contains(KeyModifiers::SHIFT))
    + 8 * u8::from(event.modifiers.contains(KeyModifiers::ALT))
    + 16 * u8::from(event.modifiers.contains(KeyModifiers::CONTROL));
  let column = u32::from(event.column) + 1;
  let row = u32::from(event.row) + 1;
  match modes.encoding() {
    MouseEncoding::Sgr => {
      let final_byte = if release { 'm' } else { 'M' };
      Some(format!("\x1b[<{code};{column};{row}{final_byte}").into_bytes())
    }
    MouseEncoding::Urxvt => Some(format!("\x1b[{};{column};{row}M", code + 32).into_bytes()),
    MouseEncoding::Utf8 => {
      // The UTF-8 extension is limited to two-byte parameters (U+07FF).
      if column + 32 > 2047 || row + 32 > 2047 {
        return None;
      }
      let mut data = String::from("\x1b[M");
      for value in [u32::from(code) + 32, column + 32, row + 32] {
        data.push(char::from_u32(value)?);
      }
      Some(data.into_bytes())
    }
    MouseEncoding::Legacy => {
      // Match tmux's handling of positions outside the legacy 223-cell range.
      Some(vec![
        27,
        b'[',
        b'M',
        code + 32,
        u8::try_from((column + 32).min(255)).ok()?,
        u8::try_from((row + 32).min(255)).ok()?,
      ])
    }
    MouseEncoding::SgrPixels => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn cursor_mode_modifiers_and_unicode_are_encoded() {
    assert_eq!(
      encode(
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        true,
        ModifyOtherKeys::Off
      ),
      b"\x1bOA"
    );
    assert_eq!(
      encode(
        KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
        true,
        ModifyOtherKeys::Off
      ),
      b"\x1b[1;5D"
    );
    assert_eq!(
      encode(
        KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE),
        false,
        ModifyOtherKeys::Off
      ),
      "界".as_bytes()
    );
    assert_eq!(
      encode(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        false,
        ModifyOtherKeys::Off
      ),
      [3]
    );
  }

  #[test]
  fn mode1_preserves_typing_alt_keys_and_well_known_control_combinations() {
    for (code, modifiers, expected) in [
      (KeyCode::Char('A'), KeyModifiers::SHIFT, b"A".as_slice()),
      (KeyCode::Char('x'), KeyModifiers::ALT, b"\x1bx".as_slice()),
      (
        KeyCode::Char('X'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
        b"\x1bX".as_slice(),
      ),
      (
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
        b"\x01".as_slice(),
      ),
      (
        KeyCode::Char('a'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        b"\x01".as_slice(),
      ),
      (KeyCode::Char(' '), KeyModifiers::CONTROL, b"\0".as_slice()),
      (KeyCode::BackTab, KeyModifiers::SHIFT, b"\x1b[Z".as_slice()),
      (
        KeyCode::Tab,
        KeyModifiers::CONTROL,
        b"\x1b[27;5;9~".as_slice(),
      ),
      (
        KeyCode::Enter,
        KeyModifiers::SHIFT,
        b"\x1b[27;2;13~".as_slice(),
      ),
      (
        KeyCode::Char('.'),
        KeyModifiers::CONTROL,
        b"\x1b[27;5;46~".as_slice(),
      ),
    ] {
      assert_eq!(
        encode(
          KeyEvent::new(code, modifiers),
          false,
          ModifyOtherKeys::Mode1
        ),
        expected,
        "{code:?}, {modifiers:?}"
      );
    }
  }

  #[test]
  fn mode2_distinguishes_modified_control_keys_and_reports_shifted_ascii() {
    for (code, modifiers, expected) in [
      (KeyCode::Char('A'), KeyModifiers::SHIFT, "A"),
      (KeyCode::Char('a'), KeyModifiers::CONTROL, "\x1b[27;5;97~"),
      (
        KeyCode::Char('A'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        "\x1b[27;6;65~",
      ),
      (KeyCode::Tab, KeyModifiers::CONTROL, "\x1b[27;5;9~"),
      (KeyCode::Char(' '), KeyModifiers::SHIFT, "\x1b[27;2;32~"),
      (
        KeyCode::BackTab,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        "\x1b[27;6;9~",
      ),
      (KeyCode::BackTab, KeyModifiers::NONE, "\x1b[27;2;9~"),
      (KeyCode::Enter, KeyModifiers::SHIFT, "\x1b[27;2;13~"),
      (KeyCode::Enter, KeyModifiers::CONTROL, "\x1b[27;5;13~"),
      (KeyCode::Backspace, KeyModifiers::CONTROL, "\x1b[27;5;127~"),
      (KeyCode::Esc, KeyModifiers::ALT, "\x1b[27;3;27~"),
    ] {
      assert_eq!(
        encode(
          KeyEvent::new(code, modifiers),
          false,
          ModifyOtherKeys::Mode2
        ),
        expected.as_bytes(),
        "{code:?}, {modifiers:?}"
      );
    }
  }

  #[test]
  fn every_mode_preserves_unmodified_text_cursor_and_function_key_sequences() {
    for mode in [
      ModifyOtherKeys::Off,
      ModifyOtherKeys::Mode1,
      ModifyOtherKeys::Mode2,
    ] {
      for (code, modifiers, expected) in [
        (KeyCode::Char('界'), KeyModifiers::NONE, "界"),
        (KeyCode::Tab, KeyModifiers::NONE, "\t"),
        (KeyCode::Enter, KeyModifiers::NONE, "\r"),
        (KeyCode::Up, KeyModifiers::NONE, "\x1bOA"),
        (KeyCode::Left, KeyModifiers::CONTROL, "\x1b[1;5D"),
        (KeyCode::PageUp, KeyModifiers::SHIFT, "\x1b[5;2~"),
        (KeyCode::F(1), KeyModifiers::NONE, "\x1bOP"),
        (KeyCode::F(4), KeyModifiers::ALT, "\x1b[1;3S"),
        (KeyCode::F(12), KeyModifiers::CONTROL, "\x1b[24;5~"),
      ] {
        assert_eq!(
          encode(KeyEvent::new(code, modifiers), true, mode),
          expected.as_bytes(),
          "{mode:?}, {code:?}, {modifiers:?}"
        );
      }
    }
  }

  #[test]
  fn unsupported_modifiers_and_key_releases_never_alias_supported_shortcuts() {
    for mode in [
      ModifyOtherKeys::Off,
      ModifyOtherKeys::Mode1,
      ModifyOtherKeys::Mode2,
    ] {
      for code in [KeyCode::Char('c'), KeyCode::Enter, KeyCode::Up] {
        for modifier in [KeyModifiers::SUPER, KeyModifiers::HYPER, KeyModifiers::META] {
          assert_eq!(
            encode(
              KeyEvent::new(code, modifier | KeyModifiers::CONTROL),
              false,
              mode
            ),
            Vec::<u8>::new()
          );
        }
        assert_eq!(
          encode(
            KeyEvent::new_with_kind(code, KeyModifiers::CONTROL, KeyEventKind::Release),
            false,
            mode
          ),
          Vec::<u8>::new()
        );
      }
    }
  }

  #[test]
  fn repeat_and_lock_state_do_not_change_the_xterm_modifier_parameter() {
    use crossterm::event::KeyEventState;
    let key = KeyEvent::new(
      KeyCode::Char('A'),
      KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    let expected = encode(key, false, ModifyOtherKeys::Mode2);
    assert_eq!(expected, b"\x1b[27;6;65~");
    for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
      let mut reported = key;
      reported.kind = kind;
      reported.state = KeyEventState::CAPS_LOCK | KeyEventState::NUM_LOCK | KeyEventState::KEYPAD;
      assert_eq!(encode(reported, false, ModifyOtherKeys::Mode2), expected);
    }
  }

  #[test]
  fn modified_unicode_preserves_the_reported_layout_character() {
    assert_eq!(
      encode(
        KeyEvent::new(KeyCode::Char('界'), KeyModifiers::CONTROL),
        false,
        ModifyOtherKeys::Mode1
      ),
      b"\x1b[27;5;30028~"
    );
    let key = KeyEvent::new(KeyCode::Char('é'), KeyModifiers::SHIFT);
    assert_eq!(encode(key, false, ModifyOtherKeys::Mode1), "é".as_bytes());
    assert_eq!(encode(key, false, ModifyOtherKeys::Mode2), "é".as_bytes());
  }

  #[test]
  fn legacy_control_symbols_cover_the_standard_digit_and_slash_aliases() {
    for (ch, expected) in [
      ('2', 0),
      ('3', 27),
      ('4', 28),
      ('5', 29),
      ('6', 30),
      ('7', 31),
      ('8', 127),
      ('/', 31),
      ('`', 0),
      ('{', 27),
      ('|', 28),
      ('}', 29),
      ('~', 30),
    ] {
      for mode in [ModifyOtherKeys::Off, ModifyOtherKeys::Mode1] {
        assert_eq!(
          encode(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL),
            false,
            mode
          ),
          [expected]
        );
      }
    }
    let question = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::CONTROL);
    assert_eq!(encode(question, false, ModifyOtherKeys::Off), [127]);
    assert_eq!(
      encode(question, false, ModifyOtherKeys::Mode1),
      b"\x1b[27;5;63~"
    );
  }

  #[test]
  fn printable_shift_only_input_stays_text_in_every_mode() {
    for mode in [
      ModifyOtherKeys::Off,
      ModifyOtherKeys::Mode1,
      ModifyOtherKeys::Mode2,
    ] {
      for (ch, expected) in [('a', "a"), ('A', "A"), ('!', "!"), ('É', "É")] {
        assert_eq!(
          encode(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT),
            false,
            mode
          ),
          expected.as_bytes()
        );
      }
    }
  }

  #[test]
  fn prefix_is_configurable_and_validated() {
    let prefix = parse_prefix("Alt+a").unwrap();
    assert!(matches_prefix(
      KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT),
      &prefix
    ));
    assert!(parse_prefix("Ctrl+ab").is_err());
    assert!(parse_prefix("Ctrl+界").is_err());
  }

  fn mouse_modes(output: &str) -> MouseModes {
    let mut modes = ctmux_core::mouse::TerminalInputModes::default();
    for ch in output.chars() {
      modes.feed(ch);
    }
    modes.mouse()
  }

  fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
      kind,
      column,
      row,
      modifiers: KeyModifiers::NONE,
    }
  }

  #[test]
  fn mouse_tracking_filters_motion_without_losing_buttons_or_wheels() {
    let down = mouse(MouseEventKind::Down(MouseButton::Left), 2, 3);
    let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 2, 3);
    let moved = mouse(MouseEventKind::Moved, 2, 3);
    let wheel = mouse(MouseEventKind::ScrollDown, 2, 3);
    assert_eq!(encode_mouse(down, MouseModes::default()), None);
    let buttons = mouse_modes("\x1b[?1000;1006h");
    assert_eq!(encode_mouse(down, buttons).unwrap(), b"\x1b[<0;3;4M");
    assert_eq!(encode_mouse(drag, buttons), None);
    assert_eq!(encode_mouse(moved, buttons), None);
    assert_eq!(encode_mouse(wheel, buttons).unwrap(), b"\x1b[<65;3;4M");
    let dragging = mouse_modes("\x1b[?1002;1006h");
    assert_eq!(encode_mouse(drag, dragging).unwrap(), b"\x1b[<32;3;4M");
    assert_eq!(encode_mouse(moved, dragging), None);
    let any = mouse_modes("\x1b[?1003;1006h");
    assert_eq!(encode_mouse(moved, any).unwrap(), b"\x1b[<35;3;4M");
  }

  #[test]
  fn sgr_preserves_released_button_modifiers_and_large_coordinates() {
    let modes = mouse_modes("\x1b[?1002;1006h");
    let mut event = mouse(MouseEventKind::Up(MouseButton::Right), 400, 65_535);
    event.modifiers = KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL;
    assert_eq!(encode_mouse(event, modes).unwrap(), b"\x1b[<30;401;65536m");
    event.kind = MouseEventKind::Drag(MouseButton::Middle);
    assert_eq!(encode_mouse(event, modes).unwrap(), b"\x1b[<61;401;65536M");
    event.kind = MouseEventKind::ScrollLeft;
    assert_eq!(encode_mouse(event, modes).unwrap(), b"\x1b[<94;401;65536M");
  }

  #[test]
  fn legacy_release_and_coordinates_match_the_supported_byte_range() {
    let modes = mouse_modes("\x1b[?1000h");
    let mut event = mouse(MouseEventKind::Up(MouseButton::Right), 222, 300);
    event.modifiers = KeyModifiers::CONTROL;
    assert_eq!(
      encode_mouse(event, modes).unwrap(),
      [27, b'[', b'M', 51, 255, 255]
    );
    event = mouse(MouseEventKind::Down(MouseButton::Middle), 0, 0);
    assert_eq!(encode_mouse(event, modes).unwrap(), b"\x1b[M!!!");
  }

  #[test]
  fn extended_protocols_are_encoded_without_silently_using_legacy_bytes() {
    let mut event = mouse(MouseEventKind::Down(MouseButton::Left), 100, 2014);
    let utf8 = mouse_modes("\x1b[?1000;1005h");
    let expected = format!("\x1b[M {}{}", '\u{85}', '\u{7ff}');
    assert_eq!(encode_mouse(event, utf8).unwrap(), expected.as_bytes());
    event.row = 2015;
    assert_eq!(encode_mouse(event, utf8), None);
    let urxvt = mouse_modes("\x1b[?1000;1015h");
    assert_eq!(encode_mouse(event, urxvt).unwrap(), b"\x1b[32;101;2016M");
    let pixels = mouse_modes("\x1b[?1000;1006;1016h");
    assert_eq!(encode_mouse(event, pixels), None);
  }
}
