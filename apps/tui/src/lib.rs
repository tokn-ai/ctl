//! Reusable local terminal UI, shared by ctmux and the legacy ctmux-tui launcher.
mod actions;
mod app;
mod copy;
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
mod terminal;
mod transport;

#[cfg(all(test, unix))]
#[path = "../tests/support/daemon.rs"]
#[allow(dead_code)]
mod test_daemon;

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
/// Returns connection, protocol, prefix validation, or terminal I/O errors.
pub async fn run(options: Options) -> Result<()> {
  ensure_terminal()?;
  let mut app = app::App::new(
    options.socket,
    options.read_only,
    input::parse_prefix(&options.prefix)?,
  );
  Box::pin(run_app(
    &mut app,
    options.archive,
    StartupMode::Standard(options.session),
  ))
  .await
}

/// Run the same terminal UI over a caller-supplied local or remote transport.
///
/// # Errors
/// Returns connection, protocol, or terminal I/O errors.
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
/// Returns connection, protocol, or terminal I/O errors.
pub async fn run_with_transport_startup(
  transport: &dyn Transport,
  startup: StartupMode,
  read_only: bool,
) -> Result<()> {
  ensure_terminal()?;
  let mut app = app::App::new(PathBuf::new(), read_only, input::parse_prefix("Ctrl+b")?);
  app.transport = Some(transport);
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
  let _interaction = TerminalInteraction::new(app.transport);
  let mut terminal = match terminal::Terminal::enter() {
    Ok(terminal) => terminal,
    Err(error) => {
      app.detach().await;
      return Err(error.into());
    }
  };
  let events = terminal.events();
  let result = tokio::select! {
    result = app.run(events) => result,
    result = shutdown_signal() => result,
  };
  app.detach().await;
  drop(terminal);
  result
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
