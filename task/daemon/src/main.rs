use clap::Parser;
use std::path::PathBuf;
use taskd::{DaemonConfig, default_data_directory, socket_path};

#[derive(Debug, Parser)]
#[command(version, about = "Per-user managed task daemon")]
struct Arguments {
  #[arg(long)]
  socket: Option<PathBuf>,

  #[arg(long)]
  data_directory: Option<PathBuf>,

  #[arg(long)]
  rmux_socket: Option<PathBuf>,

  #[arg(long, hide = true)]
  detach_from_terminal: bool,
}

fn main() {
  let arguments = Arguments::parse();
  #[cfg(unix)]
  if arguments.detach_from_terminal
    && let Err(error) = detach_from_terminal()
  {
    eprintln!("taskd: could not detach from the invoking terminal: {error}");
    std::process::exit(1);
  }

  let config = DaemonConfig {
    rmux_socket: arguments.rmux_socket.unwrap_or_else(rmux_ipc::socket_path),
    socket_path: arguments.socket.unwrap_or_else(socket_path),
    data_directory: arguments
      .data_directory
      .unwrap_or_else(default_data_directory),
  };
  let runtime = match tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
  {
    Ok(runtime) => runtime,
    Err(error) => {
      eprintln!("taskd: could not initialize the async runtime: {error}");
      std::process::exit(1);
    }
  };
  if let Err(error) = runtime.block_on(taskd::run(config)) {
    eprintln!("taskd: {error}");
    std::process::exit(1);
  }
}

#[cfg(unix)]
fn detach_from_terminal() -> std::io::Result<()> {
  rustix::process::setsid()?;
  std::env::set_current_dir("/")
}
