use ctream_proto::Display;
use std::collections::{BTreeMap, BTreeSet};
use std::{fs, io, process::Command};

fn listeners(table: &[u8]) -> BTreeSet<Display> {
  let mut displays = BTreeSet::new();
  for line in table.split(|&b| b == b'\n') {
    let Ok(line) = std::str::from_utf8(line) else {
      continue;
    };
    let mut fields = line.split_ascii_whitespace().skip(3);
    // Flags, Type, St: accepting connections, SOCK_STREAM, unconnected.
    if fields.next() != Some("00010000")
      || fields.next() != Some("0001")
      || fields.next() != Some("01")
    {
      continue;
    }
    let Some(path) = fields.nth(1) else { continue };
    if fields.next().is_some() {
      continue;
    }
    if let Some(display) = path
      .trim_start_matches('@')
      .strip_prefix("/tmp/.X11-unix/X")
      .and_then(decimal)
    {
      displays.insert(Display::X11(display));
    } else if let Some((user, socket)) = path
      .strip_prefix("/run/user/")
      .and_then(|p| p.split_once('/'))
      && let Some(uid) = decimal(user)
      && socket.strip_prefix("wayland-").and_then(decimal).is_some()
    {
      displays.insert(Display::Wayland {
        uid,
        socket: socket.to_owned(),
      });
    }
  }
  displays
}

fn decimal(value: &str) -> Option<u32> {
  value
    .bytes()
    .all(|b| b.is_ascii_digit())
    .then(|| value.parse().ok())
    .flatten()
}

fn loginctl(args: &[&str]) -> Option<String> {
  let output = Command::new("loginctl").args(args).output().ok()?;
  if !output.status.success() {
    return None;
  }
  String::from_utf8(output.stdout).ok()
}

// logind associates sessions with a user, so report a class only when the
// user's Wayland session metadata agrees. Missing metadata stays unknown.
fn wayland_class(uid: u32) -> Option<String> {
  let sessions = loginctl(&["show-user", &uid.to_string(), "-p", "Sessions", "--value"])?;
  let mut classes = BTreeSet::new();
  for session in sessions.split_ascii_whitespace() {
    let info = loginctl(&["show-session", session, "-p", "Type", "-p", "Class"])?;
    if info.lines().any(|line| line == "Type=wayland") {
      classes.insert(
        info
          .lines()
          .find_map(|line| line.strip_prefix("Class="))?
          .to_owned(),
      );
    }
  }
  if classes.len() == 1 {
    classes.pop_first()
  } else {
    None
  }
}

pub fn discover() -> io::Result<Vec<(Display, Option<String>)>> {
  let displays = listeners(&fs::read("/proc/net/unix")?);
  let mut classes = BTreeMap::new();
  Ok(
    displays
      .into_iter()
      .map(|display| {
        let class = match &display {
          Display::Wayland { uid, .. } => classes
            .entry(*uid)
            .or_insert_with(|| wayland_class(*uid))
            .clone(),
          Display::X11(_) | Display::Windows { .. } => None,
        };
        (display, class)
      })
      .collect(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lists_each_listener_once_and_preserves_wayland_owner() {
    let table = b"0: 2 0 00010000 0001 01 1 /tmp/.X11-unix/X7
0: 2 0 00010000 0001 01 2 @/tmp/.X11-unix/X7
0: 2 0 00000000 0001 03 3 /tmp/.X11-unix/X9
0: 2 0 00010000 0001 01 4 /run/user/42/wayland-0
0: 2 0 00010000 0001 01 5 /run/user/42/wayland-0.lock";
    assert_eq!(
      listeners(table),
      BTreeSet::from([
        Display::X11(7),
        Display::Wayland {
          uid: 42,
          socket: "wayland-0".into()
        }
      ])
    );
  }
}
