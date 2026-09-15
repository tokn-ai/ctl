use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(version, about = "Per-user SSH connection and credential broker")]
struct Arguments {
  #[arg(long)]
  socket: Option<PathBuf>,

  #[arg(long, hide = true)]
  detach_from_terminal: bool,
}

fn main() {
  if let Some(code) = ctld::askpass_exit_code() {
    std::process::exit(code);
  }
  let arguments = Arguments::parse();
  #[cfg(unix)]
  if arguments.detach_from_terminal
    && let Err(error) = detach_from_terminal()
  {
    eprintln!("ctld: could not detach from the invoking terminal: {error}");
    std::process::exit(1);
  }
  let runtime = match tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
  {
    Ok(runtime) => runtime,
    Err(error) => {
      eprintln!("ctld: could not initialize the async runtime: {error}");
      std::process::exit(1);
    }
  };
  if let Err(error) = runtime.block_on(ctld::run(
    arguments.socket.unwrap_or_else(ctld_ipc::socket_path),
  )) {
    eprintln!("ctld: {error}");
    std::process::exit(1);
  }
}

#[cfg(unix)]
fn detach_from_terminal() -> std::io::Result<()> {
  rustix::process::setsid()?;
  std::env::set_current_dir("/")
}
