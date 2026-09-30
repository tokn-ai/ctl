use clap::Parser;
use std::path::PathBuf;

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
  #[arg(long, default_value = "Ctrl+b", value_parser = rmux_tui::validate_prefix)]
  prefix: String,
}

#[tokio::main]
async fn main() {
  let arguments = Arguments::parse();
  if let Err(error) = rmux_tui::run(rmux_tui::Options {
    archive: None,
    socket: arguments.socket.unwrap_or_else(rmux_ipc::socket_path),
    session: arguments.session,
    read_only: arguments.read_only,
    prefix: arguments.prefix,
  })
  .await
  {
    eprintln!("rmux-tui: {error}");
    std::process::exit(1);
  }
}
