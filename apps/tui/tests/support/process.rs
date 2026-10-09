use super::{Result, Screen, TestDaemon, screen::Capture};
use filedescriptor::{AsRawFileDescriptor, FileDescriptor, RawFileDescriptor};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use std::{
  io::{self, Read, Write},
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::Duration,
};
use tokio::{sync::Notify, time::Instant};

const WAIT_LIMIT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A real TUI subprocess driven through its host terminal, rather than App events.
pub struct Tui {
  master: Option<Box<dyn MasterPty + Send>>,
  writer: Option<Box<dyn Write + Send>>,
  child: Box<dyn Child + Send + Sync>,
  exit: Option<ExitStatus>,
  capture: Arc<Mutex<Capture>>,
  changed: Arc<Notify>,
  stop: Arc<AtomicBool>,
  reader: Option<thread::JoinHandle<()>>,
}

impl Tui {
  pub async fn start(daemon: &TestDaemon, session: &str, columns: u16, rows: u16) -> Result<Self> {
    Self::start_socket(daemon, &daemon.socket, session, columns, rows).await
  }

  pub async fn start_socket(
    daemon: &TestDaemon,
    socket: &std::path::Path,
    session: &str,
    columns: u16,
    rows: u16,
  ) -> Result<Self> {
    let program = option_env!("CARGO_BIN_EXE_ctmux-tui")
      .ok_or("ctmux-tui binary is required for this launcher")?;
    let mut command = CommandBuilder::new(program);
    command.args(["--socket"]);
    command.arg(socket);
    command.arg(session);
    // Only the child sees this home and environment. Archives and shell startup
    // files cannot read or alter the developer's normal state.
    command.env_clear();
    command.env("HOME", daemon.directory.join("home"));
    command.env("PATH", "/usr/bin:/bin");
    command.env("SHELL", "/bin/sh");
    command.env("TERM", "xterm-256color");
    command.env("LANG", "C.UTF-8");
    command.cwd(&daemon.directory);
    let mut tui = Self::spawn(command, columns, rows)?;
    tui
      .wait_screen("initial connected screen", |screen| {
        screen
          .row(usize::from(rows) - 1)
          .starts_with(" connected |")
      })
      .await?;
    Ok(tui)
  }

  /// Accept a command so other TUI launchers can reuse the same PTY driver.
  pub fn spawn(command: CommandBuilder, columns: u16, rows: u16) -> Result<Self> {
    if columns == 0 || rows == 0 {
      return Err("host terminal dimensions must be nonzero".into());
    }
    let pair = native_pty_system().openpty(pty_size(columns, rows))?;
    // The reader must be interruptible: leaving a cloned master open would
    // prevent disconnect() from actually revoking the child's host terminal.
    let fd = pair
      .master
      .as_raw_fd()
      .ok_or("PTY has no native descriptor")?;
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    let reader = pair.master.try_clone_reader()?;
    // portable-pty's take_writer sends newline + EOF on drop. A plain duplicate
    // must only close, otherwise disconnect could send input to the remote shell.
    let writer = Box::new(FileDescriptor::dup(&MasterDescriptor(
      pair.master.as_ref(),
    ))?);
    let child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let capture = Arc::new(Mutex::new(Capture::new(columns, rows)));
    let changed = Arc::new(Notify::new());
    let stop = Arc::new(AtomicBool::new(false));
    let mut tui = Self {
      master: Some(pair.master),
      writer: Some(writer),
      child,
      exit: None,
      capture: Arc::clone(&capture),
      changed: Arc::clone(&changed),
      stop: Arc::clone(&stop),
      reader: None,
    };
    tui.reader = Some(
      thread::Builder::new()
        .name("tui-test-output".into())
        .spawn(move || read_output(reader, &capture, &changed, &stop))?,
    );
    Ok(tui)
  }

  pub fn screen(&self) -> Screen {
    lock(&self.capture).snapshot()
  }

  /// Inspect retained host output even if a later redraw erased it from view.
  pub fn transcript_contains(&self, bytes: &[u8]) -> bool {
    lock(&self.capture).transcript_contains(bytes)
  }

  /// Wait for terminal mode changes, including output drained after child exit.
  pub async fn wait_transcript(&mut self, description: &str, bytes: &[u8]) -> Result<()> {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
      let changed = Arc::clone(&self.changed);
      let notified = changed.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      if self.transcript_contains(bytes) {
        return Ok(());
      }
      if self.screen().closed || Instant::now() >= deadline {
        return Err(self.failure(&format!("waiting for {description}")).into());
      }
      tokio::select! {
        () = &mut notified => {}
        () = tokio::time::sleep_until(deadline) => {}
      }
    }
  }

  pub fn process_id(&self) -> Option<u32> {
    self.child.process_id()
  }

  pub fn send(&mut self, mut bytes: &[u8]) -> Result<()> {
    let deadline = Instant::now() + WAIT_LIMIT;
    while !bytes.is_empty() {
      if self.poll_exit()?.is_some() {
        return Err(self.failure("TUI exited before receiving input").into());
      }
      let result = self
        .writer
        .as_mut()
        .ok_or("host terminal disconnected")?
        .write(bytes);
      match result {
        Ok(0) => return Err(self.failure("PTY accepted no input").into()),
        Ok(count) => bytes = &bytes[count..],
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
          thread::sleep(POLL_INTERVAL);
        }
        Err(error) => return Err(self.failure(&format!("PTY input failed: {error}")).into()),
      }
    }
    Ok(())
  }

  pub async fn wait_screen(
    &mut self,
    description: &str,
    predicate: impl Fn(&Screen) -> bool,
  ) -> Result<Screen> {
    self
      .wait_screen_for(WAIT_LIMIT, description, predicate)
      .await
  }

  pub async fn wait_screen_for(
    &mut self,
    duration: Duration,
    description: &str,
    predicate: impl Fn(&Screen) -> bool,
  ) -> Result<Screen> {
    let deadline = Instant::now() + duration;
    loop {
      let changed = Arc::clone(&self.changed);
      let notified = changed.notified();
      tokio::pin!(notified);
      // Register before inspecting capture, so a fast next frame cannot lose
      // its notification between the snapshot and the await.
      notified.as_mut().enable();
      let screen = self.screen();
      let exited = self.poll_exit()?.is_some();
      if screen.closed {
        return Err(
          self
            .failure(&format!("TUI stopped while waiting for {description}"))
            .into(),
        );
      }
      // An exited child may still have a final PTY read queued. Drain it for
      // diagnostics, but never accept its retained frame as a live screen.
      if !exited && predicate(&screen) {
        return Ok(screen);
      }
      if Instant::now() >= deadline {
        return Err(self.failure(&format!("waiting for {description}")).into());
      }
      // Poll child status as well as output: startup failures may emit nothing.
      tokio::select! {
        () = &mut notified => {}
        () = tokio::time::sleep_until((Instant::now() + POLL_INTERVAL).min(deadline)) => {}
      }
    }
  }

  pub fn resize(&mut self, columns: u16, rows: u16) -> Result<()> {
    if columns == 0 || rows == 0 {
      return Err("host terminal dimensions must be nonzero".into());
    }
    // Serialize parser resizing with incoming output before SIGWINCH can
    // cause the TUI to draw a frame using the new dimensions.
    let mut capture = lock(&self.capture);
    self
      .master
      .as_ref()
      .ok_or("host terminal disconnected")?
      .resize(pty_size(columns, rows))?;
    capture.resize(columns, rows);
    Ok(())
  }

  pub async fn wait_exit(&mut self) -> Result<ExitStatus> {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
      if let Some(status) = self.poll_exit()? {
        return Ok(status);
      }
      if Instant::now() >= deadline {
        return Err(self.failure("waiting for TUI exit").into());
      }
      tokio::time::sleep(POLL_INTERVAL).await;
    }
  }

  pub fn disconnect(&mut self) -> Result<()> {
    self.stop_reader()?;
    self.writer = None;
    self.master = None;
    Ok(())
  }

  fn poll_exit(&mut self) -> io::Result<Option<ExitStatus>> {
    if self.exit.is_none() {
      self.exit = self.child.try_wait()?;
    }
    Ok(self.exit.clone())
  }

  fn failure(&self, description: &str) -> String {
    format!(
      "{description}; TUI pid {:?}, exit {:?}\n{}",
      self.child.process_id(),
      self.exit,
      lock(&self.capture).diagnostic(),
    )
  }

  fn stop_reader(&mut self) -> Result<()> {
    self.stop.store(true, Ordering::Release);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    if let Some(reader) = &self.reader {
      while !reader.is_finished() {
        if std::time::Instant::now() >= deadline {
          return Err(self.failure("output reader did not stop").into());
        }
        thread::sleep(POLL_INTERVAL);
      }
    }
    if let Some(reader) = self.reader.take() {
      reader.join().map_err(|_| "output reader panicked")?;
    }
    Ok(())
  }
}

/// Borrow the live master only long enough for a safe owned descriptor duplicate.
struct MasterDescriptor<'a>(&'a dyn MasterPty);

impl AsRawFileDescriptor for MasterDescriptor<'_> {
  fn as_raw_file_descriptor(&self) -> RawFileDescriptor {
    self.0.as_raw_fd().expect("native descriptor was validated")
  }
}

impl Drop for Tui {
  fn drop(&mut self) {
    // portable-pty's kill allows SIGHUP cleanup, then falls back to SIGKILL.
    if self.poll_exit().ok().flatten().is_none() {
      let _ = self.child.kill();
      let deadline = std::time::Instant::now() + Duration::from_secs(2);
      while self.poll_exit().ok().flatten().is_none() && std::time::Instant::now() < deadline {
        thread::sleep(POLL_INTERVAL);
      }
    }
    let _ = self.disconnect();
  }
}

fn read_output(
  mut reader: Box<dyn Read + Send>,
  capture: &Mutex<Capture>,
  changed: &Notify,
  stop: &AtomicBool,
) {
  let mut buffer = [0; 4096];
  let error = loop {
    if stop.load(Ordering::Acquire) {
      break None;
    }
    match reader.read(&mut buffer) {
      Ok(0) => break None,
      Ok(count) => {
        lock(capture).feed(&buffer[..count]);
        changed.notify_one();
      }
      Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
      Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL_INTERVAL),
      // Unix PTYs commonly use EIO to indicate that the slave has closed.
      Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => break None,
      Err(error) => break Some(error.to_string()),
    }
  };
  lock(capture).finish(error);
  changed.notify_one();
}

fn pty_size(columns: u16, rows: u16) -> PtySize {
  PtySize {
    cols: columns,
    rows,
    pixel_width: 0,
    pixel_height: 0,
  }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  mutex
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}
