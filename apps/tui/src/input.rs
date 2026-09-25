use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

/// Encode conventional xterm input, including application cursor mode.
pub fn encode(key: KeyEvent, application_cursor: bool) -> Vec<u8> {
  let modifiers = key.modifiers;
  let modified =
    modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
  let parameter = 1
    + u8::from(modifiers.contains(KeyModifiers::SHIFT))
    + 2 * u8::from(modifiers.contains(KeyModifiers::ALT))
    + 4 * u8::from(modifiers.contains(KeyModifiers::CONTROL));
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
        '@'..='_' => vec![u8::try_from(u32::from(ch)).expect("ASCII") & 0x1f],
        ' ' | '2' => vec![0],
        '?' => vec![127],
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

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn cursor_mode_modifiers_and_unicode_are_encoded() {
    assert_eq!(
      encode(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true),
      b"\x1bOA"
    );
    assert_eq!(
      encode(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL), true),
      b"\x1b[1;5D"
    );
    assert_eq!(
      encode(
        KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE),
        false
      ),
      "界".as_bytes()
    );
    assert_eq!(
      encode(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        false
      ),
      [3]
    );
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
}
