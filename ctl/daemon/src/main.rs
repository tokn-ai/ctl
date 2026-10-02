use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(version, about = "Per-user SSH connection and credential broker")]
// These are independent command-line switches, not mutable application state.
#[allow(clippy::struct_excessive_bools)]
struct Arguments {
  /// Print embedded build and protocol metadata without starting ctld.
  #[arg(long)]
  component_info: bool,
  /// Print the local IPC protocol version and exit.
  #[arg(long)]
  protocol_version: bool,

  /// Manage saved credential metadata without starting or contacting ctld.
  #[arg(long, hide = true)]
  credential_request: bool,

  /// Inspect local SSH identities or verify and manage their saved passphrases.
  #[arg(long, hide = true)]
  identity_request: bool,

  #[arg(long, hide = true)]
  identity_agent_lifetime: bool,

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
  if let Some(code) = ctld::identities::askpass_exit_code() {
    std::process::exit(code);
  }
  if let Some(code) = ctld::askpass_exit_code() {
    std::process::exit(code);
  }
  let arguments = Arguments::parse();
  if arguments.identity_agent_lifetime {
    if ctld::identities::run_lifetime().is_err() {
      std::process::exit(1);
    }
    return;
  }
  if arguments.identity_request {
    if ctld::identities::run(std::io::stdin().lock(), std::io::stdout().lock()).is_err() {
      eprintln!("ctld: identity helper I/O failed");
      std::process::exit(1);
    }
    return;
  }
  if arguments.credential_request {
    if let Err(error) = ctld::credentials::run(std::io::stdin().lock(), std::io::stdout().lock()) {
      eprintln!("ctld: credential helper I/O failed: {error}");
      std::process::exit(1);
    }
    return;
  }
  if arguments.component_info {
    let metadata = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![
        ctl_core::component::ProtocolInfo {
          name: "ctld".into(),
          version: ctl_ipc::PROTOCOL_VERSION,
        },
        ctl_core::component::ProtocolInfo {
          name: "ctld_lifecycle".into(),
          version: ctl_ipc::lifecycle::PROTOCOL_VERSION,
        },
      ],
    };
    println!(
      "{}",
      serde_json::to_string(&metadata).expect("component metadata")
    );
    return;
  }
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
    println!("{}", ctl_ipc::PROTOCOL_VERSION);
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
    arguments.socket.unwrap_or_else(ctl_ipc::socket_path),
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
