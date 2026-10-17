use base64::Engine as _;
#[cfg(unix)]
use crossterm::event::{
  KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{
  cursor::{Hide, Show},
  event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
  },
  execute,
  style::{Attribute, ResetColor, SetAttribute},
  terminal::{self, DisableLineWrap, EnableLineWrap, EnterAlternateScreen, LeaveAlternateScreen},
};
#[cfg(not(unix))]
use std::io::IsTerminal;
use std::io::{self, Write};
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
  #[cfg(unix)]
  keyboard_enhancement_attempted: bool,
}

impl Terminal {
  pub fn enter() -> io::Result<Self> {
    terminal::enable_raw_mode()?;
    let mut guard = Self {
      stop: Arc::new(AtomicBool::new(false)),
      reader: None,
      events: None,
      #[cfg(unix)]
      keyboard_enhancement_attempted: false,
    };
    execute!(
      io::stdout(),
      EnterAlternateScreen,
      DisableLineWrap,
      EnableBracketedPaste,
      EnableMouseCapture,
      Hide
    )?;
    #[cfg(unix)]
    {
      // Unsupported terminals ignore this request. Querying support would delay
      // the first screen and compete with the event reader for terminal input.
      // Mark the attempt before writing so a flush failure still restores it.
      guard.keyboard_enhancement_attempted = true;
      execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(
          KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
      )?;
    }
    let (sender, receiver) = mpsc::channel(128);
    let stop = guard.stop.clone();
    guard.events = Some(receiver);
    guard.reader = Some(std::thread::spawn(move || {
      #[cfg(unix)]
      crate::host_input::read_events(&sender, &stop);
      #[cfg(not(unix))]
      while !stop.load(Ordering::Acquire) {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
          let _ = sender.try_send(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "Terminal disconnected",
          )));
          return;
        }
        match crossterm::event::poll(Duration::from_millis(50)) {
          Ok(false) => {}
          Ok(true) => {
            let event = crossterm::event::read();
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

  /// Hand terminal input back to an interactive authentication flow.
  pub fn suspend(mut self) -> io::Result<()> {
    self.stop_reader()?;
    // Unlike best-effort shutdown, a handoff must prove cooked input is back
    // before the next owner starts a credential prompt.
    terminal::disable_raw_mode()
  }

  fn stop_reader(&mut self) -> io::Result<()> {
    self.stop.store(true, Ordering::Release);
    if let Some(reader) = self.reader.as_ref() {
      let deadline = Instant::now() + Duration::from_millis(250);
      while !reader.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
      }
      if !reader.is_finished() {
        return Err(io::Error::new(
          io::ErrorKind::TimedOut,
          "Terminal input reader did not stop; authentication was not started",
        ));
      }
    }
    if let Some(reader) = self.reader.take() {
      reader
        .join()
        .map_err(|_| io::Error::other("Terminal input reader panicked"))?;
    }
    Ok(())
  }
}

impl Drop for Terminal {
  fn drop(&mut self) {
    // Shutdown stays bounded even when a platform input backend is stalled.
    // Explicit handoff uses the fallible suspend path instead.
    let _ = self.stop_reader();
    #[cfg(unix)]
    if self.keyboard_enhancement_attempted {
      // The enhancement stack belongs to the active screen; pop before leaving
      // it, including when entry failed after the request was written.
      let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
      io::stdout(),
      SetAttribute(Attribute::Reset),
      ResetColor,
      Show,
      DisableBracketedPaste,
      DisableMouseCapture,
      EnableLineWrap,
      LeaveAlternateScreen
    );
    let _ = terminal::disable_raw_mode();
  }
}

/// Restore the host's input mode even if shutdown cancels a password prompt
/// whose blocking worker still owns its own terminal-mode guard.
#[cfg(unix)]
pub struct PromptMode {
  terminal: std::fs::File,
  mode: rustix::termios::Termios,
}

#[cfg(unix)]
impl PromptMode {
  pub fn capture() -> io::Result<Self> {
    let terminal = std::fs::File::open("/dev/tty")?;
    let mode = rustix::termios::tcgetattr(&terminal)?;
    Ok(Self { terminal, mode })
  }
}

#[cfg(unix)]
impl Drop for PromptMode {
  fn drop(&mut self) {
    let _ = rustix::termios::tcsetattr(
      &self.terminal,
      rustix::termios::OptionalActions::Now,
      &self.mode,
    );
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
