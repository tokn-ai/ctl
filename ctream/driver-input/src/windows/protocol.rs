use std::io::{self, BufRead, Read};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Key {
  Scan { code: u16, extended: bool },
  Virtual(u16),
}

fn scan(code: u16) -> Key {
  Key::Scan {
    code,
    extended: false,
  }
}

fn extended(code: u16) -> Key {
  Key::Scan {
    code,
    extended: true,
  }
}

/// Names follow the existing preview's physical-key vocabulary. Scan codes are
/// PC keyboard positions, independent of the active Windows keyboard layout.
pub(super) fn key(name: &str) -> Option<Key> {
  if name.len() == 1 {
    let byte = name.as_bytes()[0].to_ascii_lowercase();
    for (row, first) in [("qwertyuiop", 0x10), ("asdfghjkl", 0x1e), ("zxcvbnm", 0x2c)] {
      if let Some(index) = row.bytes().position(|candidate| candidate == byte) {
        return Some(scan(first + index as u16));
      }
    }
    return match byte {
      b'1'..=b'9' => Some(scan(u16::from(byte - b'1') + 2)),
      b'0' => Some(scan(0x0b)),
      _ => None,
    };
  }
  if let Some(number) = name.strip_prefix('F').and_then(|s| s.parse::<u16>().ok()) {
    return match number {
      1..=10 => Some(scan(0x3a + number)),
      11..=12 => Some(scan(0x57 + number - 11)),
      // These keys have no uniform position on a standard PC keyboard.
      13..=24 => Some(Key::Virtual(0x70 + number - 1)),
      _ => None,
    };
  }
  Some(match name {
    "Escape" => scan(0x01),
    "BackSpace" => scan(0x0e),
    "Tab" => scan(0x0f),
    "Return" => scan(0x1c),
    "space" => scan(0x39),
    "Shift_L" => scan(0x2a),
    "Shift_R" => scan(0x36),
    "Control_L" => scan(0x1d),
    "Control_R" => extended(0x1d),
    "Alt_L" => scan(0x38),
    "Alt_R" => extended(0x38),
    "Super_L" => extended(0x5b),
    "Super_R" => extended(0x5c),
    "Menu" => extended(0x5d),
    "CapsLock" | "Caps_Lock" => scan(0x3a),
    "ScrollLock" | "Scroll_Lock" => scan(0x46),
    "NumLock" | "Num_Lock" => extended(0x45),
    "Insert" => extended(0x52),
    "Delete" => extended(0x53),
    "Home" => extended(0x47),
    "End" => extended(0x4f),
    "Prior" => extended(0x49),
    "Next" => extended(0x51),
    "Left" => extended(0x4b),
    "Right" => extended(0x4d),
    "Up" => extended(0x48),
    "Down" => extended(0x50),
    "minus" | "underscore" => scan(0x0c),
    "equal" | "plus" => scan(0x0d),
    "bracketleft" | "braceleft" => scan(0x1a),
    "bracketright" | "braceright" => scan(0x1b),
    "semicolon" | "colon" => scan(0x27),
    "apostrophe" | "quotedbl" => scan(0x28),
    "grave" | "asciitilde" => scan(0x29),
    "backslash" | "bar" => scan(0x2b),
    "comma" | "less" => scan(0x33),
    "period" | "greater" => scan(0x34),
    "slash" | "question" => scan(0x35),
    "exclam" => scan(0x02),
    "PrintScreen" | "Print" => Key::Virtual(0x2c),
    "Pause" => Key::Virtual(0x13),
    "XF86Back" => Key::Virtual(0xa6),
    _ => return None,
  })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Event {
  Move { point: [u32; 2], size: [u32; 2] },
  Key(Key, bool),
  Button(u8, bool),
  Reset,
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn read_event(reader: &mut impl BufRead) -> io::Result<Option<Event>> {
  let mut line = String::new();
  let bytes = reader.take(129).read_line(&mut line)?;
  if bytes == 0 {
    return Ok(None);
  }
  if bytes > 128 || !line.ends_with('\n') {
    return Err(invalid("input command exceeds 128 bytes or is incomplete"));
  }
  parse(&line).map(Some)
}

fn parse(line: &str) -> io::Result<Event> {
  let fields: Vec<_> = line.split_ascii_whitespace().collect();
  match fields.as_slice() {
    ["reset"] => Ok(Event::Reset),
    ["move", x, y, width, height] => {
      let number = |text: &str| {
        text
          .parse::<u32>()
          .map_err(|_| invalid("invalid coordinate"))
      };
      let point = [number(x)?, number(y)?];
      let size = [number(width)?, number(height)?];
      if !valid_frame(point, size) {
        return Err(invalid("pointer is outside valid capture dimensions"));
      }
      Ok(Event::Move { point, size })
    }
    ["key", name, pressed @ ("0" | "1")] => {
      let code = key(name).ok_or_else(|| invalid("unsupported physical key"))?;
      Ok(Event::Key(code, *pressed == "1"))
    }
    ["button", code, pressed @ ("0" | "1")] => {
      let code = code.parse::<u8>().map_err(|_| invalid("invalid button"))?;
      if !(1..=9).contains(&code) {
        return Err(invalid("unknown button"));
      }
      Ok(Event::Button(code, *pressed == "1"))
    }
    _ => Err(invalid("unknown input command")),
  }
}

fn valid_frame([x, y]: [u32; 2], [width, height]: [u32; 2]) -> bool {
  (1..=16384).contains(&width)
    && (1..=16384).contains(&height)
    && u64::from(width) * u64::from(height) <= 32 * 1024 * 1024
    && x < width
    && y < height
}

/// Process-local monitor identity and geometry bound when input starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Topology {
  pub monitor_token: usize,
  pub monitor: [i32; 4],
  pub desktop: [i32; 4],
}

impl Topology {
  pub fn require_same(self, current: Self) -> io::Result<()> {
    if self != current {
      return Err(io::Error::other(
        "Windows display topology changed; reconnect before sending input",
      ));
    }
    Ok(())
  }
}

/// Return normalized SendInput coordinates at physical pixel centers. This
/// avoids rounding onto the preceding pixel and supports negative desktop origins.
pub(super) fn absolute_position(
  point: [u32; 2],
  frame: [u32; 2],
  monitor: [i32; 4],
  desktop: [i32; 4],
) -> io::Result<[i32; 2]> {
  if !valid_frame(point, frame) {
    return Err(invalid("pointer is outside valid capture dimensions"));
  }
  let mut absolute = [0; 2];
  for axis in 0..2 {
    let origin = i64::from(desktop[axis]);
    let extent = i64::from(desktop[axis + 2]) - origin;
    let start = i64::from(monitor[axis]);
    let size = i64::from(monitor[axis + 2]) - start;
    if size <= 0 || extent <= 0 || start < origin || start + size > origin + extent {
      return Err(invalid("monitor is outside the virtual desktop"));
    }
    let pixel = start + ((2 * i64::from(point[axis]) + 1) * size / (2 * i64::from(frame[axis])));
    absolute[axis] = (((2 * (pixel - origin) + 1) * 65536) / (2 * extent)).min(65535) as i32;
  }
  Ok(absolute)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mapped_pixels_round_trip_with_negative_origins_and_scaled_capture() {
    let desktop = [-1920, -1080, 2560, 1440];
    let monitor = [-1920, -1080, 0, 0];
    for point in [[0, 0], [100, 200], [959, 539]] {
      let result = absolute_position(point, [960, 540], monitor, desktop).unwrap();
      let physical = [
        i64::from(result[0]) * 4480 / 65536 - 1920,
        i64::from(result[1]) * 2520 / 65536 - 1080,
      ];
      assert_eq!(
        physical,
        [
          i64::from(point[0]) * 2 + 1 - 1920,
          i64::from(point[1]) * 2 + 1 - 1080
        ]
      );
    }
    for point in [[0, 0], [2559, 1439], [1280, 720]] {
      let result = absolute_position(point, [2560, 1440], [0, 0, 2560, 1440], desktop).unwrap();
      assert_eq!(
        i64::from(result[0]) * 4480 / 65536 - 1920,
        i64::from(point[0])
      );
      assert_eq!(
        i64::from(result[1]) * 2520 / 65536 - 1080,
        i64::from(point[1])
      );
    }
    assert!(absolute_position([0, 0], [0, 100], monitor, desktop).is_err());
    assert!(absolute_position([0, 0], [100, 100], [-2000, 0, 0, 100], desktop).is_err());
  }

  #[test]
  fn commands_are_bounded_and_malformed_events_are_rejected() {
    let mut input = &b"move 0 0 800 600\nkey Control_R 1\nreset\n"[..];
    assert!(matches!(
      read_event(&mut input).unwrap(),
      Some(Event::Move { .. })
    ));
    assert_eq!(
      read_event(&mut input).unwrap(),
      Some(Event::Key(extended(0x1d), true))
    );
    assert_eq!(read_event(&mut input).unwrap(), Some(Event::Reset));
    assert_eq!(read_event(&mut input).unwrap(), None);
    for bad in [
      "move 800 0 800 600\n",
      "move -1 0 1 1\n",
      "move 0 0 16384 16384\n",
      "key a 2\n",
      "button 0 1\n",
      "button 256 1\n",
      "reset extra\n",
      "reset",
      "key Unknown 1\n",
    ] {
      assert!(read_event(&mut bad.as_bytes()).is_err(), "{bad}");
    }
    assert!(read_event(&mut format!("{}\n", " ".repeat(128)).as_bytes()).is_err());
  }

  #[test]
  fn physical_keys_preserve_extended_codes_and_modifier_sides() {
    assert_eq!(key("a"), Some(scan(0x1e)));
    assert_eq!(key("A"), key("a"));
    assert_eq!(key("Return"), Some(scan(0x1c)));
    assert_eq!(key("Control_L"), Some(scan(0x1d)));
    assert_eq!(key("Control_R"), Some(extended(0x1d)));
    assert_eq!(key("Left"), Some(extended(0x4b)));
    assert_eq!(key("NumLock"), Some(extended(0x45)));
    assert_eq!(key("F12"), Some(scan(0x58)));
    assert_eq!(key("F24"), Some(Key::Virtual(0x87)));
    assert_eq!(key("F35"), None);
    assert_eq!(key("question"), key("slash"));
  }

  #[test]
  fn topology_binding_rejects_monitor_replacement_resize_and_desktop_relayout() {
    let original = Topology {
      monitor_token: 42,
      monitor: [0, 0, 1920, 1080],
      desktop: [-1920, 0, 1920, 1080],
    };
    assert!(original.require_same(original).is_ok());
    for changed in [
      Topology {
        monitor_token: 43,
        ..original
      },
      Topology {
        monitor: [0, 0, 1280, 720],
        ..original
      },
      Topology {
        monitor: [-1920, 0, 0, 1080],
        ..original
      },
      Topology {
        desktop: [0, 0, 1920, 1080],
        ..original
      },
    ] {
      assert!(original.require_same(changed).is_err());
    }
  }
}
