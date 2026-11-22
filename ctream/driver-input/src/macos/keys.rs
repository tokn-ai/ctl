/// Map the viewer's physical-key vocabulary to the ANSI key positions in
/// macOS HIToolbox's Events.h, independent of the host's text input layout.
pub(super) fn code(name: &str) -> Option<u16> {
  const LETTERS: [u16; 26] = [
    0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f, 0x23,
    0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
  ];
  const DIGITS: [u16; 10] = [0x1d, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19];
  const FUNCTION_KEYS: [u16; 20] = [
    0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f, 0x69, 0x6b, 0x71, 0x6a,
    0x40, 0x4f, 0x50, 0x5a,
  ];
  if let [byte] = name.as_bytes() {
    match byte.to_ascii_lowercase() {
      byte @ b'a'..=b'z' => return Some(LETTERS[usize::from(byte - b'a')]),
      byte @ b'0'..=b'9' => return Some(DIGITS[usize::from(byte - b'0')]),
      _ => {}
    }
  }
  if let Some(index) = name
    .strip_prefix('F')
    .and_then(|number| number.parse::<usize>().ok())
    .and_then(|number| number.checked_sub(1))
  {
    return FUNCTION_KEYS.get(index).copied();
  }
  Some(match name {
    "equal" | "plus" | "=" | "+" => 0x18,
    "minus" | "underscore" | "-" | "_" => 0x1b,
    "bracketright" | "braceright" | "]" | "}" => 0x1e,
    "bracketleft" | "braceleft" | "[" | "{" => 0x21,
    "Return" => 0x24,
    "apostrophe" | "quotedbl" | "'" | "\"" => 0x27,
    "semicolon" | "colon" | ";" | ":" => 0x29,
    "backslash" | "bar" | "\\" | "|" => 0x2a,
    "comma" | "less" | "," | "<" => 0x2b,
    "slash" | "question" | "/" | "?" => 0x2c,
    "period" | "greater" | "." | ">" => 0x2f,
    "Tab" => 0x30,
    "space" | " " => 0x31,
    "grave" | "asciitilde" | "`" | "~" => 0x32,
    "BackSpace" => 0x33,
    "Escape" => 0x35,
    "Super_R" => 0x36,
    "Super_L" => 0x37,
    "Shift_L" => 0x38,
    "CapsLock" | "Caps_Lock" => 0x39,
    "Alt_L" => 0x3a,
    "Control_L" => 0x3b,
    "Shift_R" => 0x3c,
    "Alt_R" => 0x3d,
    "Control_R" => 0x3e,
    "Insert" | "Help" => 0x72,
    "Home" => 0x73,
    "Prior" => 0x74,
    "Delete" => 0x75,
    "End" => 0x77,
    "Next" => 0x79,
    "Left" => 0x7b,
    "Right" => 0x7c,
    "Down" => 0x7d,
    "Up" => 0x7e,
    "exclam" | "!" => 0x12,
    _ => return None,
  })
}

/// Quartz device-independent flag OR'd with the keyboard's side-specific flag
/// from IOKit's IOLLEvent.h.
pub(super) fn modifier(code: u16) -> u64 {
  match code {
    0x36 => 0x0010_0010, // Command right
    0x37 => 0x0010_0008, // Command left
    0x38 => 0x0002_0002, // Shift left
    0x39 => 0x0001_0000, // Caps Lock
    0x3a => 0x0008_0020, // Option left
    0x3b => 0x0004_0001, // Control left
    0x3c => 0x0002_0004, // Shift right
    0x3d => 0x0008_0040, // Option right
    0x3e => 0x0004_2000, // Control right
    _ => 0,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn maps_physical_positions_and_modifier_sides() {
    assert_eq!(code("A"), Some(0x00));
    assert_eq!(code("Slash"), None);
    assert_eq!(code("question"), code("slash"));
    assert_eq!(code("BackSpace"), Some(0x33));
    assert_eq!(code("F20"), Some(0x5a));
    assert_eq!(code("F21"), None);
    let shift_left = modifier(code("Shift_L").unwrap());
    let shift_right = modifier(code("Shift_R").unwrap());
    assert_eq!(shift_left & shift_right, 0x0002_0000);
    assert_ne!(shift_left, shift_right);
  }
}
