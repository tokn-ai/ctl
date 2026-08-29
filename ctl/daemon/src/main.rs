use clap::{Parser, Subcommand};
use ctld::{DaemonConfig, create_pairing_invitation, initialize, serve, validate_listen_address};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::watch;

#[derive(Debug, Parser)]
#[command(
  version,
  about = "Authenticated ctl gateway for persistent rmux sessions"
)]
struct Arguments {
  /// Owner-only directory containing device identity and authorization state.
  #[arg(long, global = true)]
  state_dir: Option<PathBuf>,

  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Initialize this device's TLS identity and authorization registry.
  Init,

  /// Create a short-lived, one-time client pairing invitation.
  Pair {
    #[command(subcommand)]
    command: PairCommand,
  },

  /// Serve authenticated ctl connections on an explicit address.
  Serve {
    /// Concrete Tailscale or loopback address and port; wildcard addresses are rejected.
    #[arg(long)]
    listen: SocketAddr,

    /// Override the fixed local rmuxd Unix socket path.
    #[arg(long)]
    rmux_socket: Option<PathBuf>,

    /// Absolute path to a companion rmuxd binary for on-demand local startup.
    #[arg(long)]
    rmuxd_bin: Option<PathBuf>,
  },
}

#[derive(Debug, Subcommand)]
enum PairCommand {
  /// Print one pairing invitation. Treat its output as a secret until used.
  Create {
    /// Device endpoint reachable through the Tailnet, for example host.tailnet:9944.
    #[arg(long)]
    endpoint: String,

    /// Human-readable name for the client being enrolled.
    #[arg(long)]
    label: String,

    /// Number of seconds before the invitation expires.
    #[arg(long, default_value_t = 600)]
    expires_seconds: u64,
  },
}

#[tokio::main]
async fn main() {
  if let Err(error) = run(Arguments::parse()).await {
    eprintln!("ctld: {error}");
    std::process::exit(1);
  }
}

async fn run(arguments: Arguments) -> Result<(), MainError> {
  let state_dir = arguments.state_dir.unwrap_or(default_state_directory()?);
  match arguments.command {
    Command::Init => {
      let state = initialize(&state_dir)?;
      println!("initialized {} ({})", state.device_id, state.server_name);
    }
    Command::Pair {
      command:
        PairCommand::Create {
          endpoint,
          label,
          expires_seconds,
        },
    } => {
      let expiration = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .checked_add(Duration::from_secs(expires_seconds))
        .ok_or(MainError::ExpirationOverflow)?;
      let expires_at_ms =
        u64::try_from(expiration.as_millis()).map_err(|_| MainError::ExpirationOverflow)?;
      let invitation = create_pairing_invitation(&state_dir, endpoint, label, expires_at_ms)?;
      println!("{}", ctl_proto::encode_pairing_invitation(&invitation)?);
    }
    Command::Serve {
      listen,
      rmux_socket,
      rmuxd_bin,
    } => {
      validate_listen_address(listen)?;
      let mut config =
        DaemonConfig::with_defaults(state_dir, rmux_socket.unwrap_or_else(rmux_ipc::socket_path));
      config.rmuxd_bin = rmuxd_bin.or_else(companion_rmuxd_binary);
      let listener = TcpListener::bind(listen).await?;
      let (shutdown_sender, shutdown_receiver) = watch::channel(false);
      tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
          let _ = shutdown_sender.send(true);
        }
      });
      serve(listener, config, shutdown_receiver).await?;
    }
  }
  Ok(())
}

fn default_state_directory() -> Result<PathBuf, MainError> {
  if let Some(directory) = env::var_os("CTL_STATE_DIR") {
    return Ok(PathBuf::from(directory).join("device"));
  }
  if let Some(directory) = env::var_os("XDG_STATE_HOME") {
    return Ok(PathBuf::from(directory).join("ctl").join("device"));
  }
  let home = env::var_os("HOME").ok_or(MainError::MissingHomeDirectory)?;
  Ok(
    PathBuf::from(home)
      .join(".local")
      .join("state")
      .join("ctl")
      .join("device"),
  )
}

fn companion_rmuxd_binary() -> Option<PathBuf> {
  let current = env::current_exe().ok()?;
  let sibling = current.with_file_name("rmuxd");
  sibling.is_file().then_some(sibling)
}

#[derive(Debug, Error)]
enum MainError {
  #[error("could not determine a default state directory; pass --state-dir")]
  MissingHomeDirectory,
  #[error("pairing expiration is too large")]
  ExpirationOverflow,
  #[error("system time is before the Unix epoch: {0}")]
  Clock(#[from] std::time::SystemTimeError),
  #[error(transparent)]
  Daemon(#[from] ctld::DaemonError),
  #[error(transparent)]
  State(#[from] ctld::StateError),
  #[error(transparent)]
  Invitation(#[from] ctl_proto::InvitationError),
  #[error(transparent)]
  Io(#[from] std::io::Error),
}
