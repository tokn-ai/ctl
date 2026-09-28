use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ctld_ipc::{ClientMessage, ConnectError, ServerMessage, VpnStatus};

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// Start the VPN and print its SOCKS5 endpoint as JSON.
  Start {
    /// Literal VPN settings file, resolved relative to the current directory.
    #[arg(long, default_value = ".env", value_name = "PATH")]
    env_file: PathBuf,
  },
  /// Print VPN status as JSON without starting ctld.
  Status,
  /// Stop the owned VPN container, keeping ctld running.
  Stop,
}

pub async fn run(command: Command) -> Result<(), Error> {
  let (request, start_daemon, deadline) = match command {
    Command::Start { env_file } => (
      ClientMessage::StartVpn {
        env_file: std::path::absolute(env_file).map_err(Error::EnvFile)?,
      },
      true,
      Duration::from_secs(100),
    ),
    Command::Status => (ClientMessage::VpnStatus, false, Duration::from_secs(5)),
    Command::Stop => (ClientMessage::StopVpn, false, Duration::from_secs(15)),
  };
  let status = tokio::time::timeout(deadline, request_status(request, start_daemon))
    .await
    .map_err(|_| Error::Timeout)??;
  println!("{}", serde_json::to_string(&status)?);
  Ok(())
}

async fn request_status(request: ClientMessage, start_daemon: bool) -> Result<VpnStatus, Error> {
  let connected = if start_daemon {
    ctld_ipc::connect_or_start_daemon().await
  } else {
    ctld_ipc::connect_existing().await
  };
  let mut stream = match connected {
    Ok(stream) => stream,
    Err(ConnectError::Connect(error))
      if !start_daemon
        && matches!(
          error.kind(),
          io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ) =>
    {
      return Ok(VpnStatus::default());
    }
    Err(error) => return Err(error.into()),
  };
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::Handshake {
      protocol_version: ctld_ipc::PROTOCOL_VERSION,
    },
  )
  .await?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream).await? {
    Some(ServerMessage::HandshakeAccepted { protocol_version })
      if protocol_version == ctld_ipc::PROTOCOL_VERSION => {}
    Some(ServerMessage::Error { code, message }) => return Err(Error::Daemon { code, message }),
    None => return Err(Error::ConnectionClosed),
    Some(_) => return Err(Error::UnexpectedResponse),
  }
  ctld_ipc::write_frame(&mut stream, &request).await?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream).await? {
    Some(ServerMessage::VpnStatus { status }) => Ok(status),
    Some(ServerMessage::Error { code, message }) => Err(Error::Daemon { code, message }),
    None => Err(Error::ConnectionClosed),
    Some(_) => Err(Error::UnexpectedResponse),
  }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("could not resolve the VPN settings file: {0}")]
  EnvFile(#[source] io::Error),
  #[error(transparent)]
  Connect(#[from] ConnectError),
  #[error(transparent)]
  Codec(#[from] ctld_ipc::CodecError),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
  #[error("ctld closed the VPN request")]
  ConnectionClosed,
  #[error("ctld returned an unexpected response to the VPN request")]
  UnexpectedResponse,
  #[error("ctld error {code}: {message}")]
  Daemon { code: String, message: String },
  #[error("ctld VPN request timed out; use 'ctl vpn status' to check its state")]
  Timeout,
}
