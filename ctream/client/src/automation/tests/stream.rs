use super::*;
use std::{
  io::{BufRead, BufReader, PipeReader, PipeWriter},
  sync::atomic::{AtomicBool, Ordering},
  thread,
};

#[derive(Default)]
struct Host {
  capture: Mutex<Option<PipeWriter>>,
  events: Mutex<Vec<String>>,
  sync: bool,
  reject_batch: bool,
  ignore_capture_eof: AtomicBool,
}

impl Host {
  fn record(&self, event: impl Into<String>) {
    self.events.lock().unwrap().push(event.into());
  }

  fn frame(&self, header_and_pixels: &[u8]) {
    self
      .capture
      .lock()
      .unwrap()
      .as_mut()
      .unwrap()
      .write_all(header_and_pixels)
      .unwrap();
  }

  fn count(&self, event: &str) -> usize {
    self
      .events
      .lock()
      .unwrap()
      .iter()
      .filter(|item| item.as_str() == event)
      .count()
  }
}

// Each operation gets two actual pipes. The host emits sync acknowledgments
// only after reading the matching commands, and keeps idle capture open.
impl Transport for Arc<Host> {
  type Reader = PipeReader;
  type Writer = PipeWriter;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<PipeReader, PipeWriter>> {
    let operation = arguments[1].clone();
    self.record(format!("open {operation}"));
    let (mut incoming, writer) = io::pipe()?;
    let (reader, mut output) = io::pipe()?;
    let host = Arc::clone(self);
    let mut cancellation = Cancellation::default();
    match operation.as_str() {
      "capture-window" => {
        let stop = Arc::clone(self);
        cancellation = Cancellation::new(move || {
          if stop.ignore_capture_eof.load(Ordering::Acquire) {
            stop.record("cancelled capture-window");
            stop.capture.lock().unwrap().take();
          }
        });
        let mut capture = host.capture.lock().unwrap();
        assert!(capture.is_none(), "macOS allows only one active capture");
        output.write_all(b"P6\n1 1\n255\nabc")?;
        *capture = Some(output);
        drop(capture);
        thread::spawn(move || {
          io::copy(&mut incoming, &mut io::sink()).unwrap();
          host.record("closed capture-window");
          if !host.ignore_capture_eof.load(Ordering::Acquire) {
            host.capture.lock().unwrap().take();
          }
        });
      }
      "input-capabilities" => {
        serde_json::to_writer(&mut output, &serde_json::json!({"sync": self.sync}))?;
      }
      "input-sync" | "input-window" => {
        assert!(
          self.capture.lock().unwrap().is_some(),
          "macOS input requires active capture"
        );
        output.write_all(b"ready\n")?;
        thread::spawn(move || {
          let mut synced = 0;
          for line in BufReader::new(incoming).lines() {
            let line = line.unwrap();
            host.record(&line);
            if line == "sync" {
              if host.reject_batch && synced > 0 {
                output.write_all(b"denied\n").unwrap();
                return;
              }
              output.write_all(b"ok\n").unwrap();
              synced += 1;
            }
          }
          host.record(format!("closed {operation}"));
        });
      }
      _ => {
        return Err(io::Error::other(format!(
          "unexpected operation {operation}"
        )));
      }
    }
    Ok(Channel {
      reader,
      writer,
      cancellation,
    })
  }
}

fn macos(sync: bool, reject_batch: bool) -> (Automation<Arc<Host>>, Arc<Host>) {
  let host = Arc::new(Host {
    sync,
    reject_batch,
    ..Host::default()
  });
  let window = Window {
    id: "macos-7-9".into(),
    ..Window::default()
  };
  let session = Client::new(Arc::clone(&host)).session(Target::MacosDesktop, Some(window));
  (session.automation().unwrap(), host)
}

#[test]
fn persistent_macos_uses_one_capture_and_one_acknowledged_input_stream() {
  let (mut session, host) = macos(true, false);
  assert!(host.events.lock().unwrap().is_empty());
  let first = session.screenshot().unwrap();
  assert_eq!(first.pixels, b"abc");
  assert_eq!(host.count("open input-capabilities"), 0);
  session
    .with_controls(|control| control.type_text("x"))
    .unwrap();
  assert!(
    session
      .with_controls(|control| control.type_text("é"))
      .is_err()
  );
  session
    .with_controls(|control| control.type_text("y"))
    .unwrap();
  assert_eq!(host.count("open input-sync"), 1);
  assert_eq!(host.count("key y 0"), 1);

  // An idle source retains its frame. Then new pixels and dimensions replace
  // it while a previously returned snapshot stays independently readable.
  assert!(Arc::ptr_eq(&first, &session.screenshot().unwrap()));
  thread::scope(|scope| {
    scope.spawn(|| {
      thread::sleep(Duration::from_millis(20));
      host.frame(b"P6\n2 1\n255\nabcdef");
    });
    let next = session.screenshot().unwrap();
    assert_eq!(next.size, [2, 1]);
    assert_eq!(next.pixels, b"abcdef");
  });
  assert_eq!(first.pixels, b"abc");
  session.finish().unwrap();
  assert_eq!(host.count("open capture-window"), 1);
  assert!(host.events.lock().unwrap().ends_with(&[
    String::from("closed input-sync"),
    String::from("closed capture-window"),
  ]));
}

#[test]
fn retained_input_uses_eof_per_action_and_keeps_matching_capture() {
  let (mut session, host) = macos(false, false);
  for letter in ["x", "y"] {
    session
      .with_controls(|control| control.type_text(letter))
      .unwrap();
  }
  assert_eq!(host.count("open capture-window"), 1);
  assert_eq!(host.count("open input-capabilities"), 1);
  assert_eq!(host.count("open input-window"), 2);
  assert_eq!(host.count("closed input-window"), 2);
  // Input started the matching macOS capture before the first screenshot.
  // Prefer a new frame over the first, pre-input image.
  thread::scope(|scope| {
    scope.spawn(|| {
      thread::sleep(Duration::from_millis(20));
      host.frame(b"P6\n1 1\n255\nxyz");
    });
    assert_eq!(session.screenshot().unwrap().pixels, b"xyz");
  });
  session.finish().unwrap();
}

#[test]
fn input_sync_errors_are_reported_and_never_replay_an_action() {
  let (mut session, host) = macos(true, true);
  assert!(
    session
      .with_controls(|control| control.type_text("x"))
      .is_err()
  );
  assert_eq!(
    session
      .with_controls(|control| control.type_text("y"))
      .unwrap_err()
      .kind(),
    io::ErrorKind::NotConnected
  );
  assert_eq!(host.count("open input-sync"), 1);
  assert_eq!(host.count("key x 0"), 1);
  assert_eq!(host.count("key y 0"), 0);
  session.finish().unwrap();
}

#[test]
fn a_closed_or_failed_capture_overrides_the_cached_frame() {
  for damaged in [false, true] {
    let (mut session, host) = macos(true, false);
    assert_eq!(session.screenshot().unwrap().pixels, b"abc");
    if damaged {
      host.frame(b"P6\n2 2\n255\nabc");
    }
    host.capture.lock().unwrap().take();
    assert!(session.screenshot().is_err());
    assert!(
      session
        .with_controls(|control| control.type_text("a"))
        .is_err()
    );
    assert_eq!(host.count("open input-capabilities"), 0);
    assert_eq!(session.finish().is_err(), damaged);
  }
}

#[test]
fn closing_a_stalled_capture_cancels_its_reader() {
  let (mut session, host) = macos(true, false);
  host.ignore_capture_eof.store(true, Ordering::Release);
  assert_eq!(session.screenshot().unwrap().pixels, b"abc");
  assert_eq!(
    session.finish().unwrap_err().kind(),
    io::ErrorKind::TimedOut
  );
  assert_eq!(host.count("cancelled capture-window"), 1);
}
