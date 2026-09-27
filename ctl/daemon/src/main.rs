use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(version, about = "Per-user SSH connection and credential broker")]
struct Arguments {
  /// Print the local IPC protocol version and exit.
  #[arg(long)]
  protocol_version: bool,

  #[arg(long)]
  socket: Option<PathBuf>,

  #[arg(long, hide = true)]
  detach_from_terminal: bool,

  #[arg(long, hide = true)]
  proxy_route: Option<String>,
  #[arg(long, hide = true)]
  proxy_host: Option<String>,
  #[arg(long, hide = true)]
  proxy_port: Option<u16>,
}

fn main() {
  if let Some(code) = ctld::askpass_exit_code() {
    std::process::exit(code);
  }
  let arguments = Arguments::parse();
  if let Some(route) = arguments.proxy_route.as_deref() {
    let (Some(host), Some(port)) = (arguments.proxy_host.as_deref(), arguments.proxy_port) else {
      eprintln!("ctld: missing proxy destination");
      std::process::exit(2);
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("proxy runtime");
    if let Err(error) = runtime.block_on(ctld::proxy_route::run(route, host, port)) {
      eprintln!("ctld: {error}");
      std::process::exit(1);
    }
    return;
  }
  if arguments.protocol_version {
    println!("{}", ctld_ipc::PROTOCOL_VERSION);
    return;
  }
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
