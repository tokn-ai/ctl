mod app;
mod input;
mod model;
mod pane;
mod render;
mod terminal;

use clap::Parser;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Parser)]
#[command(version, about = "A tmux-style client for local rmux sessions")]
struct Arguments {
  /// Attach to a session by name or ID. Defaults to the first running session.
  session: Option<String>,
  /// Override the local Unix socket or Windows named pipe.
  #[arg(long)]
  socket: Option<PathBuf>,
  /// Observe without requesting input or resizing the shared view.
  #[arg(long)]
  read_only: bool,
  /// Command prefix: Ctrl+letter or Alt+letter.
  #[arg(long, default_value = "Ctrl+b", value_parser = input::parse_prefix)]
  prefix: input::Prefix,
}

#[tokio::main]
async fn main() {
  let arguments = Arguments::parse();
  if let Err(error) = run(arguments).await {
    eprintln!("rmux-tui: {error}");
    std::process::exit(1);
  }
}

async fn run(arguments: Arguments) -> Result<()> {
  if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
    return Err("an interactive terminal is required".into());
  }
  let mut app = app::App::new(
    arguments.socket.unwrap_or_else(rmux_ipc::socket_path),
    arguments.read_only,
    arguments.prefix,
  );
  if let Err(error) = app.start(arguments.session).await {
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
