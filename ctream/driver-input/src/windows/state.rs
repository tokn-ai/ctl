use super::protocol::{Event, Key, read_event};
use std::{
  collections::BTreeSet,
  io::{self, BufRead, Write},
  sync::mpsc::{self, Receiver, RecvTimeoutError},
  thread,
  time::{Duration, Instant},
};

pub(super) trait Backend {
  fn validate_target(&mut self) -> io::Result<()>;
  fn move_pointer(&mut self, point: [u32; 2], size: [u32; 2]) -> io::Result<()>;
  fn key(&mut self, key: Key, pressed: bool) -> io::Result<()>;
  fn button(&mut self, code: u8, pressed: bool) -> io::Result<()>;
}

struct Held<B: Backend> {
  backend: B,
  keys: BTreeSet<Key>,
  buttons: BTreeSet<u8>,
}

impl<B: Backend> Held<B> {
  fn new(backend: B) -> Self {
    Self {
      backend,
      keys: BTreeSet::new(),
      buttons: BTreeSet::new(),
    }
  }

  fn apply(&mut self, event: Event, output: &mut impl Write, synchronized: bool) -> io::Result<()> {
    match event {
      Event::Sync if synchronized => {
        self.backend.validate_target()?;
        output.write_all(b"ok\n")?;
        output.flush()
      }
      Event::Sync => Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "unknown input command",
      )),
      Event::Move { point, size } => {
        self.backend.validate_target()?;
        self.backend.move_pointer(point, size)
      }
      Event::Key(key, pressed) => {
        if self.keys.contains(&key) == pressed {
          return Ok(());
        }
        if pressed {
          self.backend.validate_target()?;
        }
        self.backend.key(key, pressed)?;
        if pressed {
          self.keys.insert(key);
        } else {
          self.keys.remove(&key);
        }
        Ok(())
      }
      Event::Button(code @ 4..=7, pressed) => {
        if pressed {
          self.backend.validate_target()?;
          self.backend.button(code, true)?;
        }
        Ok(())
      }
      Event::Button(code, pressed) => {
        if self.buttons.contains(&code) == pressed {
          return Ok(());
        }
        if pressed {
          self.backend.validate_target()?;
        }
        self.backend.button(code, pressed)?;
        if pressed {
          self.buttons.insert(code);
        } else {
          self.buttons.remove(&code);
        }
        Ok(())
      }
      Event::Reset => self.reset(),
    }
  }

  fn reset(&mut self) -> io::Result<()> {
    let mut error = None;
    for key in self.keys.clone() {
      match self.backend.key(key, false) {
        Ok(()) => {
          self.keys.remove(&key);
        }
        Err(err) => {
          error.get_or_insert(err);
        }
      }
    }
    for button in self.buttons.clone() {
      match self.backend.button(button, false) {
        Ok(()) => {
          self.buttons.remove(&button);
        }
        Err(err) => {
          error.get_or_insert(err);
        }
      }
    }
    error.map_or(Ok(()), Err)
  }
}

impl<B: Backend> Drop for Held<B> {
  fn drop(&mut self) {
    if let Err(error) = self.reset() {
      eprintln!("ctream: failed to release injected Windows input: {error}");
    }
  }
}

pub(super) fn run(
  synchronized: bool,
  backend: impl Backend,
  input: &mut impl BufRead,
  output: &mut impl Write,
) -> io::Result<()> {
  let mut state = Held::new(backend);
  output.write_all(b"ready\n")?;
  output.flush()?;
  while let Some(event) = read_event(input)? {
    state.apply(event, output, synchronized)?;
  }
  state.reset()
}

/// Stdin is process-scoped. Reading it separately lets an idle window session
/// release held input promptly when its target disappears. On error the agent
/// exits; a pending stdin read cannot delay target cleanup or process shutdown.
pub(super) fn run_monitored(
  synchronized: bool,
  backend: impl Backend,
  mut input: impl BufRead + Send + 'static,
  output: &mut impl Write,
) -> io::Result<()> {
  let (sender, receiver) = mpsc::sync_channel(32);
  thread::Builder::new()
    .name("ctream-window-input".into())
    .spawn(move || {
      loop {
        let event = read_event(&mut input);
        let done = !matches!(event, Ok(Some(_)));
        if sender.send(event).is_err() || done {
          break;
        }
      }
    })?;
  run_received(synchronized, backend, receiver, output)
}

fn run_received(
  synchronized: bool,
  backend: impl Backend,
  receiver: Receiver<io::Result<Option<Event>>>,
  output: &mut impl Write,
) -> io::Result<()> {
  let mut state = Held::new(backend);
  output.write_all(b"ready\n")?;
  output.flush()?;
  let mut next_check = Instant::now() + Duration::from_millis(50);
  loop {
    if Instant::now() >= next_check {
      state.backend.validate_target()?;
      next_check = Instant::now() + Duration::from_millis(50);
    }
    match receiver.recv_timeout(next_check.saturating_duration_since(Instant::now())) {
      Ok(Ok(Some(event))) => state.apply(event, output, synchronized)?,
      Ok(Ok(None)) | Err(RecvTimeoutError::Disconnected) => break,
      Ok(Err(error)) => return Err(error),
      Err(RecvTimeoutError::Timeout) => continue,
    }
  }
  state.reset()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{cell::RefCell, rc::Rc};

  struct Fake {
    events: Rc<RefCell<Vec<Event>>>,
    fail_key_down: bool,
    fail_key_up_once: bool,
    invalidate_after: Option<usize>,
  }
  impl Backend for Fake {
    fn validate_target(&mut self) -> io::Result<()> {
      if self
        .invalidate_after
        .is_some_and(|limit| self.events.borrow().len() >= limit)
      {
        return Err(io::Error::other("display topology changed"));
      }
      Ok(())
    }
    fn move_pointer(&mut self, point: [u32; 2], size: [u32; 2]) -> io::Result<()> {
      self.events.borrow_mut().push(Event::Move { point, size });
      Ok(())
    }
    fn key(&mut self, key: Key, pressed: bool) -> io::Result<()> {
      if self.fail_key_down && pressed {
        return Err(io::Error::other("injection failed"));
      }
      if !pressed && std::mem::take(&mut self.fail_key_up_once) {
        return Err(io::Error::other("release failed"));
      }
      self.events.borrow_mut().push(Event::Key(key, pressed));
      Ok(())
    }
    fn button(&mut self, code: u8, pressed: bool) -> io::Result<()> {
      self.events.borrow_mut().push(Event::Button(code, pressed));
      Ok(())
    }
  }

  #[test]
  fn sync_acknowledges_dispatched_events_only_on_the_new_channel() {
    for (synchronized, fail_key_down, expected) in [
      (false, false, b"ready\n".as_slice()),
      (true, false, b"ready\nok\nok\n".as_slice()),
      (true, true, b"ready\nok\n".as_slice()),
    ] {
      let backend = Fake {
        events: Rc::default(),
        fail_key_down,
        fail_key_up_once: false,
        invalidate_after: None,
      };
      let mut output = Vec::new();
      let result = run_monitored(
        synchronized,
        backend,
        io::Cursor::new(b"sync\nkey a 1\nsync\n"),
        &mut output,
      );
      assert_eq!(result.is_ok(), synchronized && !fail_key_down);
      assert_eq!(output, expected);
    }
  }

  #[test]
  fn eof_and_invalid_requests_release_only_successfully_injected_inputs() {
    let key = super::super::protocol::key("a").unwrap();
    for ending in ["", "bad\n"] {
      let events = Rc::new(RefCell::new(Vec::new()));
      let backend = Fake {
        events: events.clone(),
        fail_key_down: false,
        fail_key_up_once: false,
        invalidate_after: None,
      };
      let data = format!(
        "key b 0\nbutton 3 0\nkey a 1\nkey a 1\nbutton 1 1\nbutton 4 1\nbutton 4 0\n{ending}"
      );
      let mut output = Vec::new();
      assert_eq!(
        run(false, backend, &mut data.as_bytes(), &mut output).is_ok(),
        ending.is_empty()
      );
      assert_eq!(output, b"ready\n");
      assert_eq!(
        *events.borrow(),
        [
          Event::Key(key, true),
          Event::Button(1, true),
          Event::Button(4, true),
          Event::Key(key, false),
          Event::Button(1, false)
        ]
      );
    }
    let events = Rc::new(RefCell::new(Vec::new()));
    let backend = Fake {
      events: events.clone(),
      fail_key_down: true,
      fail_key_up_once: false,
      invalidate_after: None,
    };
    assert!(
      run(
        false,
        backend,
        &mut &b"button 1 1\nkey a 1\n"[..],
        &mut Vec::new()
      )
      .is_err()
    );
    assert_eq!(
      *events.borrow(),
      [Event::Button(1, true), Event::Button(1, false)]
    );
  }

  #[test]
  fn reset_releases_input_once_and_accepts_another_press() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let backend = Fake {
      events: events.clone(),
      fail_key_down: false,
      fail_key_up_once: false,
      invalidate_after: None,
    };
    run(
      false,
      backend,
      &mut &b"button 8 1\nreset\nreset\nbutton 8 1\n"[..],
      &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(
      *events.borrow(),
      [
        Event::Button(8, true),
        Event::Button(8, false),
        Event::Button(8, true),
        Event::Button(8, false)
      ]
    );
  }

  #[test]
  fn a_release_failure_still_releases_other_inputs_and_is_retried_on_drop() {
    let key = super::super::protocol::key("a").unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let backend = Fake {
      events: events.clone(),
      fail_key_down: false,
      fail_key_up_once: true,
      invalidate_after: None,
    };
    assert!(
      run(
        false,
        backend,
        &mut &b"key a 1\nbutton 1 1\nreset\n"[..],
        &mut Vec::new(),
      )
      .is_err()
    );
    assert_eq!(
      *events.borrow(),
      [
        Event::Key(key, true),
        Event::Button(1, true),
        Event::Button(1, false),
        Event::Key(key, false),
      ]
    );
  }

  #[test]
  fn stale_targets_block_new_input_but_preserve_releases_and_reset() {
    let key = super::super::protocol::key("a").unwrap();
    for blocked in [
      "move 1 1 800 600\n",
      "key b 1\n",
      "button 3 1\n",
      "button 4 1\n",
    ] {
      let events = Rc::new(RefCell::new(Vec::new()));
      let backend = Fake {
        events: events.clone(),
        fail_key_down: false,
        fail_key_up_once: false,
        invalidate_after: Some(2),
      };
      let data = format!("key a 1\nbutton 1 1\n{blocked}");
      assert!(run(false, backend, &mut data.as_bytes(), &mut Vec::new()).is_err());
      assert_eq!(
        *events.borrow(),
        [
          Event::Key(key, true),
          Event::Button(1, true),
          Event::Key(key, false),
          Event::Button(1, false)
        ]
      );
    }
    for release in ["key a 0\nbutton 1 0\n", "reset\n"] {
      let events = Rc::new(RefCell::new(Vec::new()));
      let backend = Fake {
        events: events.clone(),
        fail_key_down: false,
        fail_key_up_once: false,
        invalidate_after: Some(2),
      };
      let data = format!("key a 1\nbutton 1 1\n{release}");
      run(false, backend, &mut data.as_bytes(), &mut Vec::new()).unwrap();
      assert_eq!(
        *events.borrow(),
        [
          Event::Key(key, true),
          Event::Button(1, true),
          Event::Key(key, false),
          Event::Button(1, false)
        ]
      );
    }
  }

  #[test]
  fn idle_window_liveness_failure_releases_held_inputs_without_stdin_eof() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let backend = Fake {
      events: events.clone(),
      fail_key_down: false,
      fail_key_up_once: false,
      invalidate_after: Some(2),
    };
    let key = super::super::protocol::key("F8").unwrap();
    let (sender, receiver) = mpsc::channel();
    sender.send(Ok(Some(Event::Key(key, true)))).unwrap();
    sender.send(Ok(Some(Event::Button(1, true)))).unwrap();
    assert!(run_received(false, backend, receiver, &mut Vec::new()).is_err());
    assert_eq!(
      *events.borrow(),
      [
        Event::Key(key, true),
        Event::Button(1, true),
        Event::Key(key, false),
        Event::Button(1, false),
      ]
    );
    drop(sender);
  }

  #[test]
  fn monitored_input_still_handles_handshake_events_and_eof() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let backend = Fake {
      events: events.clone(),
      fail_key_down: false,
      fail_key_up_once: false,
      invalidate_after: None,
    };
    let mut output = Vec::new();
    run_monitored(
      false,
      backend,
      io::Cursor::new(b"button 1 1\n"),
      &mut output,
    )
    .unwrap();
    assert_eq!(output, b"ready\n");
    assert_eq!(
      *events.borrow(),
      [Event::Button(1, true), Event::Button(1, false)]
    );
  }
}
