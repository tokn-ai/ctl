use base64::Engine as _;
use crossterm::{
  cursor::{Hide, Show},
  event::{self, DisableBracketedPaste, EnableBracketedPaste, Event},
  execute,
  style::{Attribute, ResetColor, SetAttribute},
  terminal::{self, DisableLineWrap, EnableLineWrap, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, IsTerminal, Write};
use std::sync::{
  Arc,
  atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct Terminal {
  stop: Arc<AtomicBool>,
  reader: Option<std::thread::JoinHandle<()>>,
  events: Option<mpsc::Receiver<io::Result<Event>>>,
}

impl Terminal {
  pub fn enter() -> io::Result<Self> {
    terminal::enable_raw_mode()?;
    let mut guard = Self {
      stop: Arc::new(AtomicBool::new(false)),
      reader: None,
      events: None,
    };
    execute!(
      io::stdout(),
      EnterAlternateScreen,
      DisableLineWrap,
      EnableBracketedPaste,
      Hide
    )?;
    let (sender, receiver) = mpsc::channel(128);
    let stop = guard.stop.clone();
    guard.events = Some(receiver);
    guard.reader = Some(std::thread::spawn(move || {
      while !stop.load(Ordering::Acquire) {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
          let _ = sender.try_send(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "Terminal disconnected",
          )));
          return;
        }
        match event::poll(Duration::from_millis(50)) {
          Ok(false) => {}
          Ok(true) => {
            let event = event::read();
            let failed = event.is_err();
            // Keep the input thread bounded and responsive to terminal teardown.
            let mut pending = event;
            loop {
              match sender.try_send(pending) {
                Ok(()) => break,
                Err(mpsc::error::TrySendError::Closed(_)) => return,
                Err(mpsc::error::TrySendError::Full(value)) => {
                  if stop.load(Ordering::Acquire) {
                    return;
                  }
                  pending = value;
                  std::thread::sleep(Duration::from_millis(5));
                }
              }
            }
            if failed {
              return;
            }
          }
          Err(error) => {
            let _ = sender.try_send(Err(error));
            return;
          }
        }
      }
    }));
    Ok(guard)
  }

  pub fn events(&mut self) -> mpsc::Receiver<io::Result<Event>> {
    self.events.take().expect("terminal events taken once")
  }
}

impl Drop for Terminal {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Release);
    if let Some(reader) = self.reader.take() {
      // Do not let a platform input backend prevent process shutdown. Healthy
      // readers observe stop within the 50ms poll timeout; allow scheduling slack.
      let deadline = Instant::now() + Duration::from_millis(250);
      while !reader.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
      }
      if reader.is_finished() {
        let _ = reader.join();
      }
    }
    let _ = execute!(
      io::stdout(),
      SetAttribute(Attribute::Reset),
      ResetColor,
      Show,
      DisableBracketedPaste,
      EnableLineWrap,
      LeaveAlternateScreen
    );
    let _ = terminal::disable_raw_mode();
  }
}

/// OSC 52 writes only on an explicit copy action. The internal buffer also works
/// in terminals that disable clipboard escapes or impose a smaller size limit.
pub fn copy_to_clipboard(text: &str) -> io::Result<bool> {
  if text.len() > 100_000 {
    return Ok(false);
  }
  let payload = base64::engine::general_purpose::STANDARD.encode(text);
  let mut output = io::stdout().lock();
  write!(output, "\x1b]52;c;{payload}\x07")?;
  output.flush()?;
  Ok(true)
}
