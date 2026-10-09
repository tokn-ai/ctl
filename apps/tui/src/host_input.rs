//! Bounded Unix terminal framing around terminput's public event parser.
use crossterm::event::Event;
use filedescriptor::{POLLERR, POLLHUP, POLLIN, poll, pollfd};
use rustix::fs::{Mode, OFlags, open};
use std::fs::File;
use std::io::{self, IsTerminal, Read};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const ESC_DELAY: Duration = Duration::from_millis(30);
const MAX_SEQUENCE: usize = 256;
const MAX_PASTE: usize = 1024 * 1024;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

pub fn read_events(sender: &mpsc::Sender<io::Result<Event>>, stop: &AtomicBool) {
  if let Err(error) = read_terminal(sender, stop) {
    let _ = sender.try_send(Err(error));
  }
}

fn read_terminal(sender: &mpsc::Sender<io::Result<Event>>, stop: &AtomicBool) -> io::Result<()> {
  let mut tty = File::from(open(
    "/dev/tty",
    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
    Mode::empty(),
  )?);
  let mut decoder = Decoder::default();
  let mut size = crossterm::terminal::size()?;
  // App construction may precede a slow attach. Publish the size measured
  // when this reader starts, even when no subsequent SIGWINCH arrives.
  if !send_event(sender, stop, Event::Resize(size.0, size.1)) {
    return Ok(());
  }
  let mut measured = Instant::now();
  let mut bytes = [0; 1024];
  while !stop.load(Ordering::Acquire) {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
      return Err(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "Terminal disconnected",
      ));
    }
    if measured.elapsed() >= POLL_INTERVAL {
      let next = crossterm::terminal::size()?;
      measured = Instant::now();
      if next != size {
        size = next;
        if !send_event(sender, stop, Event::Resize(size.0, size.1)) {
          return Ok(());
        }
      }
    }
    if let Some(event) = decoder.expire(Instant::now())
      && !send_event(sender, stop, event)
    {
      return Ok(());
    }
    let mut descriptors = [pollfd {
      fd: tty.as_raw_fd(),
      events: POLLIN,
      revents: 0,
    }];
    match poll(&mut descriptors, Some(decoder.wait(Instant::now()))) {
      Ok(0) => continue,
      Ok(_) => {}
      Err(filedescriptor::Error::Poll(error)) if error.kind() == io::ErrorKind::Interrupted => {
        continue;
      }
      Err(error) => return Err(io::Error::other(error)),
    }
    if descriptors[0].revents & (POLLERR | POLLHUP) != 0 {
      return Err(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "Terminal disconnected",
      ));
    }
    match tty.read(&mut bytes) {
      Ok(0) => {
        return Err(io::Error::new(
          io::ErrorKind::UnexpectedEof,
          "Terminal input ended",
        ));
      }
      Ok(count) => {
        for event in decoder.feed(&bytes[..count], Instant::now()) {
          if !send_event(sender, stop, event) {
            return Ok(());
          }
        }
      }
      Err(error)
        if matches!(
          error.kind(),
          io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) => {}
      Err(error) => return Err(error),
    }
  }
  Ok(())
}

fn send_event(sender: &mpsc::Sender<io::Result<Event>>, stop: &AtomicBool, event: Event) -> bool {
  let mut pending = Ok(event);
  loop {
    match sender.try_send(pending) {
      Ok(()) => return true,
      Err(mpsc::error::TrySendError::Closed(_)) => return false,
      Err(mpsc::error::TrySendError::Full(value)) => {
        if stop.load(Ordering::Acquire) {
          return false;
        }
        pending = value;
        std::thread::sleep(Duration::from_millis(5));
      }
    }
  }
}

#[derive(Default)]
struct Decoder {
  frame: Vec<u8>,
  escape_at: Option<Instant>,
  discard: Option<Discard>,
}

enum Discard {
  Csi,
  Paste(Vec<u8>),
}

impl Decoder {
  fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<Event> {
    let mut events = Vec::new();
    if let Some(event) = self.expire(now) {
      events.push(event);
    }
    for &byte in bytes {
      if self.discard_byte(byte) {
        continue;
      }
      if self.frame == b"\x1b\x1b" && byte != b'[' {
        self.finish(&mut events);
      }
      self.frame.push(byte);
      if self.frame == b"\x1b" || self.frame == b"\x1b\x1b" {
        self.escape_at = Some(now);
        continue;
      }
      self.escape_at = None;
      if self.frame == b"\x1b[" || self.frame == b"\x1bO" {
        self.escape_at = Some(now);
        continue;
      }
      if self.frame.starts_with(PASTE_START) {
        if self.frame.ends_with(PASTE_END) {
          if self.frame.len() - PASTE_START.len() - PASTE_END.len() <= MAX_PASTE {
            self.finish(&mut events);
          } else {
            self.frame.clear();
          }
        } else if self.frame.len() > MAX_PASTE + PASTE_START.len() + PASTE_END.len() {
          self.discard = Some(Discard::Paste(
            self.frame[self.frame.len() - PASTE_END.len()..].to_vec(),
          ));
          self.frame.clear();
        }
        continue;
      }
      if self.frame.starts_with(b"\x1b[") {
        if self.frame == b"\x1b[[" {
          continue;
        } else if self.frame.get(2) == Some(&b'M') {
          if self.frame.len() == 6 {
            self.finish(&mut events);
          }
        } else if self.frame.len() > 2 && (0x40..=0x7e).contains(&byte) {
          self.finish(&mut events);
        } else if self.frame.len() >= MAX_SEQUENCE {
          self.frame.clear();
          self.discard = Some(Discard::Csi);
        }
        continue;
      }
      if self.frame == b"\x1bO" || self.frame == b"\x1b\x1b[" {
        continue;
      }
      // A parser error consumes this entire frame, never its parameter bytes as
      // ordinary input. UTF-8 waits for the complete scalar across read calls.
      match terminput::Event::parse_from(&self.frame) {
        Ok(Some(_)) => self.finish(&mut events),
        Ok(None) => {}
        Err(_) => self.frame.clear(),
      }
      if self.frame.len() >= MAX_SEQUENCE {
        self.frame.clear();
      }
    }
    events
  }

  fn discard_byte(&mut self, byte: u8) -> bool {
    let Some(discard) = self.discard.as_mut() else {
      return false;
    };
    let done = match discard {
      Discard::Csi => (0x40..=0x7e).contains(&byte),
      Discard::Paste(tail) => {
        tail.push(byte);
        if tail.len() > PASTE_END.len() {
          tail.remove(0);
        }
        tail == PASTE_END
      }
    };
    if done {
      self.discard = None;
    }
    true
  }

  fn finish(&mut self, events: &mut Vec<Event>) {
    if let Some(event) = parse_frame(&self.frame) {
      events.push(event);
    }
    self.frame.clear();
  }

  fn expire(&mut self, now: Instant) -> Option<Event> {
    let started = self.escape_at?;
    if now.duration_since(started) < ESC_DELAY {
      return None;
    }
    self.escape_at = None;
    let event = match self.frame.as_slice() {
      b"\x1b[" | b"\x1bO" => {
        let mut event = parse_frame(&self.frame[1..]);
        if let Some(Event::Key(key)) = event.as_mut() {
          key.modifiers.insert(crossterm::event::KeyModifiers::ALT);
        }
        event
      }
      _ => parse_frame(&self.frame),
    };
    self.frame.clear();
    event
  }

  fn wait(&self, now: Instant) -> Duration {
    self.escape_at.map_or(POLL_INTERVAL, |at| {
      ESC_DELAY
        .saturating_sub(now.duration_since(at))
        .min(POLL_INTERVAL)
    })
  }
}

fn parse_frame(frame: &[u8]) -> Option<Event> {
  // terminput uses one-based SGR mouse coordinates. Invalid zero coordinates
  // must be rejected before its subtraction (including debug builds).
  if frame.starts_with(b"\x1b[<") && !valid_mouse_coordinates(&frame[3..]) {
    return None;
  }
  if frame.starts_with(b"\x1b[")
    && frame.ends_with(b"M")
    && frame.get(2).is_some_and(u8::is_ascii_digit)
    && !valid_mouse_coordinates(&frame[2..])
  {
    return None;
  }
  let rewritten = rewrite_csi_u(frame);
  let event = terminput::Event::parse_from(rewritten.as_deref().unwrap_or(frame)).ok()??;
  terminput_crossterm::to_crossterm(event).ok()
}

fn valid_mouse_coordinates(frame: &[u8]) -> bool {
  let Ok(parameters) = std::str::from_utf8(&frame[..frame.len().saturating_sub(1)]) else {
    return false;
  };
  let values: Vec<_> = parameters.split(';').collect();
  values.len() == 3
    && values[1..]
      .iter()
      .all(|value| value.parse::<u16>().is_ok_and(|number| number > 0))
}

/// Preserve the layout's actual shifted character and the physical Shift flag.
/// The public parser otherwise substitutes the alternate and removes Shift.
fn rewrite_csi_u(frame: &[u8]) -> Option<Vec<u8>> {
  let body = std::str::from_utf8(frame.strip_prefix(b"\x1b[")?.strip_suffix(b"u")?).ok()?;
  let (codes, suffix) = body.split_once(';').unwrap_or((body, ""));
  let mut alternates = codes.split(':');
  let primary = alternates.next()?.parse::<u32>().ok()?;
  let shifted = alternates
    .next()
    .and_then(|value| value.parse::<u32>().ok())
    .and_then(char::from_u32);
  let modifier = suffix
    .split([';', ':'])
    .next()
    .and_then(|value| value.parse::<u8>().ok())
    .unwrap_or(1);
  let shift = modifier.saturating_sub(1) & 1 != 0;
  let chosen = if shift {
    shifted.map_or_else(
      || {
        char::from_u32(primary).map_or(primary, |character| {
          u32::from(character.to_ascii_uppercase())
        })
      },
      u32::from,
    )
  } else {
    primary
  };
  let suffix = if suffix.is_empty() {
    String::new()
  } else {
    format!(";{suffix}")
  };
  Some(format!("\x1b[{chosen}{suffix}u").into_bytes())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEventKind,
  };

  fn keys(bytes: &[u8]) -> Vec<KeyEvent> {
    Decoder::default()
      .feed(bytes, Instant::now())
      .into_iter()
      .map(|event| {
        let Event::Key(key) = event else {
          panic!("expected key, got {event:?}");
        };
        key
      })
      .collect()
  }

  #[test]
  fn shifted_layout_characters_preserve_modifiers_and_locks() {
    let values = keys(b"\x1b[97:65;6u\x1b[49:33;4u\x1b[233:201;6u\x1b[97:97;70u");
    assert_eq!(
      values.iter().map(|key| key.code).collect::<Vec<_>>(),
      vec![
        KeyCode::Char('A'),
        KeyCode::Char('!'),
        KeyCode::Char('É'),
        KeyCode::Char('a')
      ]
    );
    assert_eq!(
      values[0].modifiers,
      KeyModifiers::CONTROL | KeyModifiers::SHIFT
    );
    assert_eq!(values[1].modifiers, KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(
      values[2].modifiers,
      KeyModifiers::CONTROL | KeyModifiers::SHIFT
    );
    assert_eq!(
      values[3].modifiers,
      KeyModifiers::CONTROL | KeyModifiers::SHIFT
    );
    assert_eq!(values[3].state, KeyEventState::CAPS_LOCK);
  }

  #[test]
  fn alternate_is_used_only_for_shift_and_ascii_fallback_is_local_to_csi_u() {
    let values = keys(b"\x1b[97:65:113;5u\x1b[97;6u\x1b[49;4uA\x1b[97:0;6u");
    assert_eq!(values[0].code, KeyCode::Char('a'));
    assert_eq!(values[0].modifiers, KeyModifiers::CONTROL);
    assert_eq!(values[1].code, KeyCode::Char('A'));
    assert_eq!(
      values[1].modifiers,
      KeyModifiers::CONTROL | KeyModifiers::SHIFT
    );
    assert_eq!(values[2].code, KeyCode::Char('1'));
    assert_eq!(values[3].code, KeyCode::Char('A'));
    assert_eq!(values[4].code, KeyCode::Char('\0'));
  }

  #[test]
  fn control_kinds_functional_keys_and_legacy_input_survive_conversion() {
    let values =
      keys(b"\x1b[97:65;6:2u\x1b[97:65;6:3u\x1b[9;2u\x1b[A\x1bOP\x1b[[B\x02\x1b!\x1bP\x1b]");
    assert_eq!(values[0].kind, KeyEventKind::Repeat);
    assert_eq!(values[1].kind, KeyEventKind::Release);
    assert_eq!(values[2].code, KeyCode::BackTab);
    assert_eq!(values[3].code, KeyCode::Up);
    assert_eq!(values[4].code, KeyCode::F(1));
    assert_eq!(values[5].code, KeyCode::F(2));
    assert_eq!(
      values[6],
      KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)
    );
    assert_eq!(
      values[7],
      KeyEvent::new(KeyCode::Char('!'), KeyModifiers::ALT)
    );
    assert_eq!(values[8].code, KeyCode::Char('P'));
    assert_eq!(values[8].modifiers, KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(
      values[9],
      KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)
    );
  }

  #[test]
  fn fragments_wait_for_complete_unicode_and_csi_events() {
    let mut decoder = Decoder::default();
    let now = Instant::now();
    assert_eq!(decoder.feed(&[0xc3], now), Vec::<Event>::new());
    assert_eq!(
      decoder.feed(&[0xa9], now),
      vec![Event::Key(KeyEvent::new(
        KeyCode::Char('é'),
        KeyModifiers::NONE
      ))]
    );
    assert_eq!(decoder.feed(b"\x1b", now), Vec::<Event>::new());
    assert_eq!(decoder.feed(b"[49:33;", now), Vec::<Event>::new());
    assert_eq!(
      decoder.feed(b"4u", now),
      vec![Event::Key(KeyEvent::new(
        KeyCode::Char('!'),
        KeyModifiers::ALT | KeyModifiers::SHIFT
      ))]
    );
    assert_eq!(decoder.feed(b"\x1b\xc3", now), Vec::<Event>::new());
    assert_eq!(
      decoder.feed(b"\xa9", now),
      vec![Event::Key(KeyEvent::new(
        KeyCode::Char('é'),
        KeyModifiers::ALT
      ))]
    );
  }

  #[test]
  fn escape_has_a_bounded_deadline() {
    let mut decoder = Decoder::default();
    let now = Instant::now();
    assert_eq!(decoder.feed(b"\x1b", now), Vec::<Event>::new());
    assert!(decoder.expire(now + ESC_DELAY / 2).is_none());
    assert_eq!(
      decoder.expire(now + ESC_DELAY),
      Some(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
    );
    assert!(decoder.expire(now + POLL_INTERVAL).is_none());
    assert_eq!(decoder.feed(b"\x1b\x1b", now), Vec::<Event>::new());
    assert_eq!(
      decoder.expire(now + ESC_DELAY),
      Some(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::ALT)))
    );
    assert_eq!(
      decoder.feed(b"\x1b\x1b[Z", now),
      vec![Event::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::ALT | KeyModifiers::SHIFT
      ))]
    );
    for (bytes, code, modifiers) in [
      (b"\x1b[", KeyCode::Char('['), KeyModifiers::ALT),
      (
        b"\x1bO",
        KeyCode::Char('O'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
      ),
    ] {
      assert_eq!(decoder.feed(bytes, now), Vec::<Event>::new());
      assert_eq!(
        decoder.expire(now + ESC_DELAY),
        Some(Event::Key(KeyEvent::new(code, modifiers)))
      );
    }
    assert_eq!(decoder.feed(b"\x1b[99;", now), Vec::<Event>::new());
    assert!(decoder.expire(now + POLL_INTERVAL).is_none());
    assert_eq!(
      decoder.feed(b"2~", now + POLL_INTERVAL),
      Vec::<Event>::new()
    );
  }

  #[test]
  fn paste_and_sgr_mouse_are_single_events_across_reads() {
    let mut decoder = Decoder::default();
    let now = Instant::now();
    assert_eq!(
      decoder.feed(b"\x1b[200~hello\n\x02\x1b[", now),
      Vec::<Event>::new()
    );
    assert_eq!(
      decoder.feed(b"201~", now),
      vec![Event::Paste("hello\n\x02".to_owned())]
    );
    assert_eq!(decoder.feed(b"\x1b[<32;12;", now), Vec::<Event>::new());
    let events = decoder.feed(b"7M", now);
    let [Event::Mouse(mouse)] = events.as_slice() else {
      panic!("expected mouse");
    };
    assert_eq!(
      (mouse.column, mouse.row, mouse.kind),
      (11, 6, MouseEventKind::Drag(MouseButton::Left))
    );
  }

  #[test]
  fn unknown_sequences_and_invalid_mouse_coordinates_do_not_leak() {
    let values = keys(b"\x1b[999;2~\x1b[?5u\x1b[<0;0;1M\x1b[32;1;0Mz");
    assert_eq!(
      values,
      vec![KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)]
    );
  }

  #[test]
  fn oversized_csi_and_paste_are_discarded_through_their_terminator() {
    let mut decoder = Decoder::default();
    let now = Instant::now();
    assert_eq!(decoder.feed(b"\x1b[", now), Vec::<Event>::new());
    assert_eq!(
      decoder.feed(&vec![b'1'; MAX_SEQUENCE * 2], now),
      Vec::<Event>::new()
    );
    assert_eq!(
      decoder.feed(b";5uz", now),
      vec![Event::Key(KeyEvent::new(
        KeyCode::Char('z'),
        KeyModifiers::NONE
      ))]
    );
    assert_eq!(decoder.feed(PASTE_START, now), Vec::<Event>::new());
    assert_eq!(
      decoder.feed(&vec![b'x'; MAX_PASTE + 32], now),
      Vec::<Event>::new()
    );
    assert!(decoder.frame.len() <= MAX_SEQUENCE);
    assert_eq!(decoder.feed(b"\n\x02\x1b[20", now), Vec::<Event>::new());
    assert_eq!(
      decoder.feed(b"1~q", now),
      vec![Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::NONE
      ))]
    );
  }

  #[test]
  fn paste_limit_applies_to_payload_even_when_the_terminator_completes() {
    for length in [MAX_PASTE, MAX_PASTE + 1] {
      let mut decoder = Decoder::default();
      let now = Instant::now();
      assert_eq!(decoder.feed(PASTE_START, now), Vec::<Event>::new());
      assert_eq!(decoder.feed(&vec![b'x'; length], now), Vec::<Event>::new());
      let events = decoder.feed(PASTE_END, now);
      if length == MAX_PASTE {
        assert_eq!(events, vec![Event::Paste("x".repeat(MAX_PASTE))]);
      } else {
        assert_eq!(events, Vec::<Event>::new());
      }
      assert_eq!(
        decoder.feed(b"q", now),
        vec![Event::Key(KeyEvent::new(
          KeyCode::Char('q'),
          KeyModifiers::NONE
        ))]
      );
    }
  }

  #[test]
  fn full_event_queue_observes_shutdown() {
    let (sender, _receiver) = mpsc::channel(1);
    sender.try_send(Ok(Event::FocusGained)).unwrap();
    assert!(!send_event(
      &sender,
      &AtomicBool::new(true),
      Event::FocusLost
    ));
  }
}
