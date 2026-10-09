use clap::Parser;
use ctmuxd::{DaemonConfig, run};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(version, about = "Persistent local terminal session daemon")]
struct Arguments {
  /// Print component version/build/protocol metadata without starting the daemon.
  #[arg(long)]
  component_info: bool,
  /// Override the local endpoint (Unix socket or Windows named pipe).
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

  /// Detach from the invoking terminal. Used by ctmux auto-start.
  #[arg(long, hide = true)]
  detach_from_terminal: bool,
}

fn main() {
  let arguments = Arguments::parse();
  if arguments.component_info {
    let info = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![
        ctmux_proto::protocol_info(),
        ctmux_ipc::local_control_protocol_info(),
      ],
    };
    println!(
      "{}",
      serde_json::to_string(&info).expect("component metadata serializes")
    );
    return;
  }
  ctl_core::observability::initialize_daemon(
    ctl_core::observability::Component::Ctmuxd,
    arguments.detach_from_terminal,
  );
  let lifecycle = ctl_core::observability::Operation::diagnostic(
    "53150eae-6ffe-4ee8-b043-e5cec8ab5b83",
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

  let config = DaemonConfig {
    socket_path: arguments.socket.unwrap_or_else(ctmux_ipc::socket_path),
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
      lifecycle.finish(
        ctl_core::observability::Outcome::Failed,
        Some("daemon_runtime_failed"),
        error.raw_os_error(),
      );
      std::process::exit(1);
    }
  };

  let result = runtime.block_on(run(config));
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
