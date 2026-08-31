use clap::{Parser, Subcommand};
use ctld::{ConnectConfig, connect_stdio};
use std::env;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(version, about = "SSH remote-command gateway for rmux")]
struct Arguments {
  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Relay this SSH channel's standard streams to the local rmux daemon.
  Connect,
}

#[tokio::main]
async fn main() {
  if let Err(error) = run(Arguments::parse()).await {
    eprintln!("ctld: {error}");
    std::process::exit(1);
  }
}

async fn run(arguments: Arguments) -> Result<(), MainError> {
  match arguments.command {
    Command::Connect => {
      let mut config = ConnectConfig::new(rmux_ipc::socket_path());
      config.rmuxd_bin = companion_rmuxd_binary();
      connect_stdio(&config).await?;
    }
  }
  Ok(())
}

fn companion_rmuxd_binary() -> Option<PathBuf> {
  let current = env::current_exe().ok()?;
  let sibling = current.with_file_name("rmuxd");
  sibling.is_file().then_some(sibling)
}

#[derive(Debug, Error)]
enum MainError {
  #[error(transparent)]
  Daemon(#[from] ctld::DaemonError),
}
