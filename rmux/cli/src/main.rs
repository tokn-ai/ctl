use clap::Parser;
use rmux_cli::Command;
use rmux_cli::LocalConnector;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(version, about = "Persistent local terminal sessions")]
struct Arguments {
  /// Override the local endpoint (Unix socket or Windows named pipe).
  #[arg(long, global = true)]
  socket: Option<PathBuf>,

  #[command(subcommand)]
  command: Command,
}

#[tokio::main]
async fn main() {
  let arguments = Arguments::parse();
  let connector = LocalConnector::new(arguments.socket.unwrap_or_else(rmux_ipc::socket_path));
  if let Err(error) = rmux_cli::run(arguments.command, &connector).await {
    eprintln!("rmux: {error}");
    std::process::exit(1);
  }
}
