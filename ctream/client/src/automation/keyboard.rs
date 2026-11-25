use super::invalid;
use crate::input::valid_key_name;
use std::io;

pub(super) fn validate_chord(keys: &[String]) -> io::Result<()> {
  if !(1..=32).contains(&keys.len()) {
    return Err(invalid("a chord requires 1 to 32 keys"));
  }
  for (index, name) in keys.iter().enumerate() {
    if !valid_key_name(name) {
      return Err(invalid("invalid physical key name"));
    }
    if keys[..index].contains(name) {
      return Err(invalid("duplicate key in chord"));
    }
  }
  Ok(())
}

pub(super) fn us_key(byte: u8) -> (&'static str, bool) {
  const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
  const DIGITS: &str = "0123456789";
  if byte.is_ascii_alphabetic() {
    let index = usize::from(byte.to_ascii_lowercase() - b'a');
    return (&LETTERS[index..=index], byte.is_ascii_uppercase());
  }
  if byte.is_ascii_digit() {
    let index = usize::from(byte - b'0');
    return (&DIGITS[index..=index], false);
  }
  match byte {
    b' ' => ("space", false),
    b'\t' => ("Tab", false),
    b'\r' | b'\n' => ("Return", false),
    b')' => ("0", true),
    b'!' => ("1", true),
    b'@' => ("2", true),
    b'#' => ("3", true),
    b'$' => ("4", true),
    b'%' => ("5", true),
    b'^' => ("6", true),
    b'&' => ("7", true),
    b'*' => ("8", true),
    b'(' => ("9", true),
    b'`' | b'~' => ("grave", byte == b'~'),
    b'-' | b'_' => ("minus", byte == b'_'),
    b'=' | b'+' => ("equal", byte == b'+'),
    b'[' | b'{' => ("bracketleft", byte == b'{'),
    b']' | b'}' => ("bracketright", byte == b'}'),
    b'\\' | b'|' => ("backslash", byte == b'|'),
    b';' | b':' => ("semicolon", byte == b':'),
    b'\'' | b'"' => ("apostrophe", byte == b'"'),
    b',' | b'<' => ("comma", byte == b'<'),
    b'.' | b'>' => ("period", byte == b'>'),
    b'/' | b'?' => ("slash", byte == b'?'),
    _ => unreachable!("type_text validates all bytes before sending input"),
  }
}
