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
  /// Print the internal wire contract build and exit.
  #[arg(long)]
  protocol_build: bool,

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
  if ["CTLD_ASKPASS", "CTLD_IDENTITY_ASKPASS"]
    .iter()
    .any(|name| std::env::var(name).as_deref() == Ok("1"))
  {
    ctl_core::observability::initialize();
  }
  if let Some(code) = ctld::identities::askpass_exit_code() {
    std::process::exit(code);
  }
  if let Some(code) = ctld::askpass_exit_code() {
    std::process::exit(code);
  }
  let arguments = Arguments::parse();
  if !arguments.component_info && !arguments.protocol_build && !arguments.protocol_version {
    if arguments.credential_request
      || arguments.identity_request
      || arguments.identity_agent_lifetime
      || arguments.proxy_route.is_some()
    {
      ctl_core::observability::initialize();
    } else {
      ctl_core::observability::initialize_daemon(
        ctl_core::observability::Component::Ctld,
        arguments.detach_from_terminal,
      );
    }
  }
  if arguments.identity_agent_lifetime {
    if ctld::identities::run_lifetime().is_err() {
      std::process::exit(1);
    }
    return;
  }
  if arguments.identity_request || arguments.credential_request {
    if run_helper(arguments.identity_request).is_err() {
      std::process::exit(1);
    }
    return;
  }
  if arguments.component_info {
    print_component_info();
    return;
  }
  if let Some(route) = arguments.proxy_route.as_deref() {
    let operation = ctl_core::observability::Operation::diagnostic(
      "718cbec3-56e7-4ac7-adba-6c67810105d6",
      ctl_core::observability::Event::ProxyConnection,
    );
    let (Some(host), Some(port)) = (arguments.proxy_host.as_deref(), arguments.proxy_port) else {
      operation.finish(
        ctl_core::observability::Outcome::Failed,
        Some("proxy_destination_missing"),
        None,
      );
      std::process::exit(2);
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
    {
      Ok(runtime) => runtime,
      Err(error) => {
        operation.finish(
          ctl_core::observability::Outcome::Failed,
          Some("daemon_runtime_failed"),
          error.raw_os_error(),
        );
        std::process::exit(1);
      }
    };
    let result = runtime.block_on(ctld::proxy_route::run(route, host, port));
    operation.finish(
      if result.is_ok() {
        ctl_core::observability::Outcome::Succeeded
      } else {
        ctl_core::observability::Outcome::Failed
      },
      result.as_ref().err().map(|_| "proxy_connection_failed"),
      None,
    );
    if result.is_err() {
      std::process::exit(1);
    }
    return;
  }
  if arguments.protocol_build {
    println!("{}", ctl_ipc::PROTOCOL_BUILD);
    return;
  }
  if arguments.protocol_version {
    println!("{}", ctl_ipc::PROTOCOL_VERSION);
    return;
  }
  run_daemon(arguments);
}

fn print_component_info() {
  let metadata = ctl_core::component::ComponentInfo {
    build: ctl_core::component::build_info(),
    protocols: ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols,
  };
  println!(
    "{}",
    serde_json::to_string(&metadata).expect("component metadata")
  );
}

fn run_helper(identity: bool) -> std::io::Result<()> {
  use ctl_core::observability::{Event, Operation, Outcome};
  let operation =
    Operation::diagnostic("029181a0-f47e-488b-8d6a-c09dff63ac45", Event::HelperRequest);
  let result = if identity {
    ctld::identities::run(std::io::stdin().lock(), std::io::stdout().lock())
  } else {
    ctld::credentials::run(std::io::stdin().lock(), std::io::stdout().lock())
  };
  operation.finish(
    if result.is_ok() {
      Outcome::Succeeded
    } else {
      Outcome::Failed
    },
    result.as_ref().err().map(|_| {
      if identity {
        "identity_helper_io_failed"
      } else {
        "credential_helper_io_failed"
      }
    }),
    result.as_ref().err().and_then(std::io::Error::raw_os_error),
  );
  result
}

fn run_daemon(arguments: Arguments) {
  let lifecycle = ctl_core::observability::Operation::diagnostic(
    "128fc95c-6bfa-47d8-b2aa-18d5278b5ada",
    ctl_core::observability::Event::DaemonLifecycle,
  );
  #[cfg(unix)]
  if arguments.detach_from_terminal
    && let Err(error) = detach_from_terminal()
  {
    lifecycle.finish(
      ctl_core::observability::Outcome::Failed,
      Some("daemon_detach_failed"),
      error.raw_os_error(),
    );
    std::process::exit(1);
  }
  let runtime = match tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
  {
    Ok(runtime) => runtime,
    Err(error) => {
      lifecycle.finish(
        ctl_core::observability::Outcome::Failed,
        Some("daemon_runtime_failed"),
        error.raw_os_error(),
      );
      std::process::exit(1);
    }
  };
  let socket = arguments.socket.unwrap_or_else(ctl_ipc::socket_path);
  #[cfg(target_os = "macos")]
  let result = ctld::run_monitored(runtime, socket);
  #[cfg(not(target_os = "macos"))]
  let result = runtime.block_on(ctld::run(socket));
  lifecycle.finish(
    if result.is_ok() {
      ctl_core::observability::Outcome::Succeeded
    } else {
      ctl_core::observability::Outcome::Failed
    },
    result.as_ref().err().map(|error| error.diagnostic().0),
    result.as_ref().err().and_then(|error| error.diagnostic().1),
  );
  if result.is_err() {
    std::process::exit(1);
  }
}

#[cfg(unix)]
fn detach_from_terminal() -> std::io::Result<()> {
  rustix::process::setsid()?;
  std::env::set_current_dir("/")
}
