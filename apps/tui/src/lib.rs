//! Reusable local terminal UI, shared by ctmux and the legacy ctmux-tui launcher.
mod actions;
mod app;
mod copy;
mod deadline;
#[cfg(test)]
mod deadline_tests;
mod divider;
#[cfg(unix)]
mod host_input;
mod input;
mod keys;
mod maintenance;
mod model;
mod pane;
mod prompt;
mod render;
mod settings;
mod terminal;
mod transport;

#[cfg(all(test, unix))]
#[path = "../tests/support/daemon.rs"]
#[allow(dead_code)]
mod test_daemon;

pub use settings::{AfterCopy, MouseRelease, SelectionSettings, Settings};
pub use transport::{ConnectFuture, Duplex, Stream, Transport};

/// How the terminal UI starts before opening any live pane attachments.
pub enum StartupMode {
  /// Select an existing target, or open/create the standalone launcher's default.
  Standard(Option<String>),
  /// Browse existing sessions; create a shell only when no sessions exist.
  Resume,
}

use std::io::{self, IsTerminal};
use std::path::PathBuf;

/// Error returned by the terminal client.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
/// Result returned by the terminal client.
pub type Result<T> = std::result::Result<T, Error>;

/// Local connection and presentation settings.
pub struct Options {
  pub socket: PathBuf,
  pub archive: Option<String>,
  pub session: Option<String>,
  pub read_only: bool,
  pub prefix: String,
}

/// Validate a configurable prefix before any session is created.
///
/// # Errors
/// Rejects unsupported modifiers and non-letter keys.
pub fn validate_prefix(value: &str) -> std::result::Result<String, String> {
  input::parse_prefix(value).map(|_| value.to_owned())
}

/// Check terminal availability before creating an interactive session.
///
/// # Errors
/// Returns an error when stdin or stdout is redirected.
pub fn ensure_terminal() -> Result<()> {
  if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
    return Err("an interactive terminal is required; use new -d for a detached session".into());
  }
  Ok(())
}

/// Run the local split-pane UI and restore the terminal on exit.
///
/// # Errors
/// Returns configuration, connection, protocol, prefix validation, or terminal I/O errors.
pub async fn run(mut options: Options) -> Result<()> {
  let session = options.session.take();
  run_with_startup(options, StartupMode::Standard(session)).await
}

/// Run the local UI with explicit startup, retaining socket, prefix, and archive settings.
/// The startup policy takes precedence over `options.session`.
///
/// # Errors
/// Returns configuration, connection, protocol, prefix validation, or terminal I/O errors.
pub async fn run_with_startup(options: Options, startup: StartupMode) -> Result<()> {
  ensure_terminal()?;
  run_with_startup_and_settings(options, startup, Settings::load()?).await
}

/// Run the local UI using settings already validated by the launcher.
///
/// Launchers that create a session before entering the UI must load settings first
/// and pass that same value here so invalid configuration cannot leave a new session.
///
/// # Errors
/// Returns connection, protocol, prefix validation, or terminal I/O errors.
pub async fn run_with_startup_and_settings(
  options: Options,
  startup: StartupMode,
  settings: Settings,
) -> Result<()> {
  ensure_terminal()?;
  let mut app = app::App::new(
    options.socket,
    options.read_only,
    input::parse_prefix(&options.prefix)?,
  );
  app.selection_settings = settings.selection;
  Box::pin(run_app(&mut app, options.archive, startup)).await
}

/// Run the same terminal UI over a caller-supplied local or remote transport.
///
/// # Errors
/// Returns configuration, connection, protocol, or terminal I/O errors.
pub async fn run_with_transport(
  transport: &dyn Transport,
  session: Option<String>,
  read_only: bool,
) -> Result<()> {
  run_with_transport_startup(transport, StartupMode::Standard(session), read_only).await
}

/// Run the terminal UI with an explicit session startup policy.
///
/// # Errors
/// Returns configuration, connection, protocol, or terminal I/O errors.
pub async fn run_with_transport_startup(
  transport: &dyn Transport,
  startup: StartupMode,
  read_only: bool,
) -> Result<()> {
  ensure_terminal()?;
  run_with_transport_startup_and_settings(transport, startup, read_only, Settings::load()?).await
}

/// Run over a caller-supplied transport using settings validated before session creation.
///
/// # Errors
/// Returns connection, protocol, or terminal I/O errors.
pub async fn run_with_transport_startup_and_settings(
  transport: &dyn Transport,
  startup: StartupMode,
  read_only: bool,
  settings: Settings,
) -> Result<()> {
  ensure_terminal()?;
  let mut app = app::App::new(PathBuf::new(), read_only, input::parse_prefix("Ctrl+b")?);
  app.transport = Some(transport);
  app.selection_settings = settings.selection;
  Box::pin(run_app(&mut app, None, startup)).await
}

async fn run_app(
  app: &mut app::App<'_>,
  archive: Option<String>,
  startup: StartupMode,
) -> Result<()> {
  let started = if let Some(id) = archive {
    app.open_archive(&id)
  } else {
    match startup {
      StartupMode::Standard(session) => app.start(session).await,
      StartupMode::Resume => app.start_resume().await,
    }
  };
  if let Err(error) = started {
    app.detach().await;
    return Err(error);
  }
  let result = tokio::select! {
    result = run_terminal(app) => result,
    result = shutdown_signal() => result,
  };
  app.detach().await;
  result
}

async fn run_terminal(app: &mut app::App<'_>) -> Result<()> {
  loop {
    let interaction = TerminalInteraction::new(app.transport);
    let mut terminal = terminal::Terminal::enter()?;
    app.run(terminal.events()).await?;
    let Some(reconnect) = app.take_interactive_reconnect() else {
      return Ok(());
    };
    // Stop and join the input reader before any credential prompt can read the
    // host terminal. Restore cooked mode and the primary screen first as well.
    terminal.suspend()?;
    drop(interaction);
    let result = {
      let _mode = terminal::PromptMode::capture()?;
      reconnect.await
    };
    app.finish_interactive_reconnect(result);
  }
}

struct TerminalInteraction<'a>(Option<&'a dyn Transport>);

impl<'a> TerminalInteraction<'a> {
  fn new(transport: Option<&'a dyn Transport>) -> Self {
    if let Some(transport) = transport {
      transport.set_terminal_ui_active(true);
    }
    Self(transport)
  }
}

impl Drop for TerminalInteraction<'_> {
  fn drop(&mut self) {
    if let Some(transport) = self.0 {
      transport.set_terminal_ui_active(false);
    }
  }
}

async fn shutdown_signal() -> Result<()> {
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    tokio::select! {
      result = tokio::signal::ctrl_c() => result?,
      _ = terminate.recv() => {}
      _ = hangup.recv() => {}
    }
  }
  #[cfg(not(unix))]
  tokio::signal::ctrl_c().await?;
  Ok(())
}
