use clap::Parser;
use rmuxd::{DaemonConfig, run};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(version, about = "Persistent local terminal session daemon")]
struct Arguments {
  /// Override the local Unix socket path.
  #[arg(long)]
  socket: Option<PathBuf>,

  /// Maximum number of raw output bytes retained per session.
  #[arg(long, default_value_t = 4 * 1024 * 1024)]
  journal_bytes: usize,

  /// Maximum output bytes between terminal-state checkpoints.
  #[arg(long, default_value_t = 256 * 1024)]
  checkpoint_bytes: usize,

  /// Exit after this many idle seconds if no session was created.
  #[arg(long, default_value_t = 10)]
  startup_idle_seconds: u64,

  /// Release an attached client's leases after this many silent seconds.
  #[arg(long, default_value_t = 30)]
  attachment_liveness_seconds: u64,

  /// Detach from the invoking terminal. Used by rmux auto-start.
  #[arg(long, hide = true)]
  detach_from_terminal: bool,
}

fn main() {
  let arguments = Arguments::parse();
  if arguments.detach_from_terminal
    && let Err(error) = detach_from_terminal()
  {
    eprintln!("rmuxd: could not detach from the invoking terminal: {error}");
    std::process::exit(1);
  }

  let config = DaemonConfig {
    socket_path: arguments.socket.unwrap_or_else(rmux_ipc::socket_path),
    journal_capacity_bytes: arguments.journal_bytes,
    checkpoint_interval_bytes: arguments.checkpoint_bytes,
    startup_idle_timeout: Duration::from_secs(arguments.startup_idle_seconds),
    attachment_liveness_timeout: Duration::from_secs(arguments.attachment_liveness_seconds),
  };

  let runtime = match tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
  {
    Ok(runtime) => runtime,
    Err(error) => {
      eprintln!("rmuxd: could not initialize the async runtime: {error}");
      std::process::exit(1);
    }
  };

  if let Err(error) = runtime.block_on(run(config)) {
    eprintln!("rmuxd: {error}");
    std::process::exit(1);
  }
}

#[cfg(unix)]
fn detach_from_terminal() -> std::io::Result<()> {
  rustix::process::setsid()?;
  std::env::set_current_dir("/")
}
