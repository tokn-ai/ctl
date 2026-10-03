use clap::Parser;
use ctl_taskd::{DaemonConfig, default_data_directory, socket_path};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(version, about = "Per-user managed task daemon")]
struct Arguments {
  /// Print component version/build/protocol metadata without starting the daemon.
  #[arg(long)]
  component_info: bool,
  #[arg(long)]
  socket: Option<PathBuf>,

  #[arg(long)]
  data_directory: Option<PathBuf>,

  #[arg(long)]
  ctmux_socket: Option<PathBuf>,

  #[arg(long, hide = true)]
  detach_from_terminal: bool,
}

fn main() {
  let arguments = Arguments::parse();
  if arguments.component_info {
    let info = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![
        ctl_task_proto::protocol_info(),
        ctl_task_proto::control::protocol_info(),
      ],
    };
    println!(
      "{}",
      serde_json::to_string(&info).expect("component metadata serializes")
    );
    return;
  }
  #[cfg(unix)]
  if arguments.detach_from_terminal
    && let Err(error) = detach_from_terminal()
  {
    eprintln!("ctl-taskd: could not detach from the invoking terminal: {error}");
    std::process::exit(1);
  }

  let data_directory = match arguments
    .data_directory
    .map_or_else(default_data_directory, Ok)
  {
    Ok(directory) => directory,
    Err(error) => {
      eprintln!("ctl-taskd: could not locate the task state directory: {error}");
      std::process::exit(1);
    }
  };
  let config = DaemonConfig {
    ctmux_socket: arguments
      .ctmux_socket
      .unwrap_or_else(ctmux_ipc::socket_path),
    socket_path: arguments.socket.unwrap_or_else(socket_path),
    data_directory,
  };
  let runtime = match tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
  {
    Ok(runtime) => runtime,
    Err(error) => {
      eprintln!("ctl-taskd: could not initialize the async runtime: {error}");
      std::process::exit(1);
    }
  };
  if let Err(error) = runtime.block_on(ctl_taskd::run(config)) {
    eprintln!("ctl-taskd: {error}");
    std::process::exit(1);
  }
}

#[cfg(unix)]
fn detach_from_terminal() -> std::io::Result<()> {
  rustix::process::setsid()?;
  std::env::set_current_dir("/")
}
