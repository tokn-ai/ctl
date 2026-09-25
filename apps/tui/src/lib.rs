//! Reusable local terminal UI, shared by rmux and the legacy rmux-tui launcher.
mod app;
mod input;
mod model;
mod pane;
mod render;
mod terminal;

use std::io::{self, IsTerminal};
use std::path::PathBuf;

/// Error returned by the terminal client.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
/// Result returned by the terminal client.
pub type Result<T> = std::result::Result<T, Error>;

/// Local connection and presentation settings.
pub struct Options {
  pub socket: PathBuf,
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
  if let Err(error) = app.start(options.session).await {
    app.detach().await;
    return Err(error);
  }
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
