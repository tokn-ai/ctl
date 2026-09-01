use clap::Parser;
use rmux_cli::Command;
#[cfg(unix)]
use rmux_cli::LocalConnector;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(version, about = "Persistent local terminal sessions")]
struct Arguments {
  /// Override the local Unix socket path.
  #[arg(long, global = true)]
  socket: Option<PathBuf>,

  #[command(subcommand)]
  command: Command,
}

#[cfg(unix)]
#[tokio::main]
async fn main() {
  let arguments = Arguments::parse();
  let connector = LocalConnector::new(arguments.socket.unwrap_or_else(rmux_ipc::socket_path));
  if let Err(error) = rmux_cli::run(arguments.command, &connector).await {
    eprintln!("rmux: {error}");
    std::process::exit(1);
  }
}

#[cfg(not(unix))]
fn main() {
  eprintln!("rmux: local IPC is not yet implemented on this platform");
  std::process::exit(1);
}
