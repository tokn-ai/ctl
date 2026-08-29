//! Authenticated `ctl` gateway for the local persistent `rmuxd` service.
//!
//! `ctld` authenticates an outer TLS connection, authorizes exactly one
//! service, then relays raw `rmux-proto` bytes to the fixed local endpoint.
//! It never owns a PTY, a shell, terminal history, or a remote session.

#[cfg(not(unix))]
compile_error!("ctld local rmux transport is currently implemented only for Unix platforms");

mod state;

pub use state::{
  AuthorizedClient, DEVICE_STATE_VERSION, DeviceState, PendingPairing, StateError, consume_pairing,
  create_pairing_invitation, initialize, load,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use ctl_proto::{
  CONTROL_PROTOCOL_VERSION, Capability, ClientMessage, CodecError, ErrorCode, ServerMessage,
  Service, authentication_payload, client_id_from_public_key, read_frame, write_frame,
};
use ed25519_dalek::{Signature, VerifyingKey};
use rustls::{
  ServerConfig,
  pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, timeout};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const RMUX_START_TIMEOUT: Duration = Duration::from_secs(3);

/// Configuration for a running `ctld` instance.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
  /// Owner-only directory containing device identity and authorization state.
  pub state_dir: PathBuf,
  /// Fixed per-user local `rmuxd` endpoint. It is never client controlled.
  pub rmux_socket: PathBuf,
  /// Absolute installed `rmuxd` path used only when the endpoint is absent.
  pub rmuxd_bin: Option<PathBuf>,
  /// Bound for TLS and outer-control handshake operations.
  pub control_timeout: Duration,
}

impl DaemonConfig {
  #[must_use]
  pub fn with_defaults(state_dir: PathBuf, rmux_socket: PathBuf) -> Self {
    Self {
      state_dir,
      rmux_socket,
      rmuxd_bin: None,
      control_timeout: DEFAULT_CONTROL_TIMEOUT,
    }
  }
}

/// Rejects an accidental public default bind address.
///
/// Tailscale reachability is intentionally configured outside this function;
/// callers must choose a concrete device or loopback address explicitly.
///
/// # Errors
///
/// Returns an error for wildcard IPv4 or IPv6 addresses.
pub fn validate_listen_address(address: SocketAddr) -> Result<(), DaemonError> {
  if address.ip().is_unspecified() {
    return Err(DaemonError::WildcardListenAddress(address));
  }
  Ok(())
}

/// Builds the TLS server configuration pinned by pairing invitations.
///
/// # Errors
///
/// Returns an error if device state/key material is absent, unsafe, or does
/// not form a usable TLS identity.
pub fn tls_server_config(state_dir: &Path) -> Result<ServerConfig, DaemonError> {
  let state = load(state_dir)?;
  let certificate = STANDARD.decode(state.server_certificate_base64)?;
  let key = state::load_server_key(state_dir)?;
  let private_key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key));
  Ok(
    ServerConfig::builder()
      .with_no_client_auth()
      .with_single_cert(vec![CertificateDer::from(certificate)], private_key)?,
  )
}

/// Serves authenticated control connections until `shutdown` is signalled.
///
/// Every accepted connection has its own bounded TLS/control setup. Once the
/// caller selects `rmux`, the stream is upgraded permanently to a raw byte
/// relay. On shutdown, relay tasks are aborted so their local `rmuxd`
/// attachments disconnect and release connection-bound leases.
///
/// # Errors
///
/// Returns an error when configuration is unsafe, TLS setup fails, or the
/// listener cannot accept a new connection.
pub async fn serve(
  listener: TcpListener,
  config: DaemonConfig,
  mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), DaemonError> {
  validate_listen_address(listener.local_addr()?)?;
  let tls = TlsAcceptor::from(Arc::new(tls_server_config(&config.state_dir)?));
  let config = Arc::new(config);
  let mut tasks = JoinSet::new();

  loop {
    tokio::select! {
      changed = shutdown.changed() => {
        if changed.is_err() || *shutdown.borrow() {
          break;
        }
      }
      accepted = listener.accept() => {
        let (stream, _) = accepted?;
        let tls = tls.clone();
        let config = Arc::clone(&config);
        tasks.spawn(async move { handle_connection(stream, tls, config).await });
      }
      Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
        match joined {
          Ok(Ok(())) | Err(_) => {}
          Ok(Err(error)) => eprintln!("ctld connection error: {error}"),
        }
      }
    }
  }

  tasks.abort_all();
  while tasks.join_next().await.is_some() {}
  Ok(())
}

async fn handle_connection(
  stream: TcpStream,
  tls: TlsAcceptor,
  config: Arc<DaemonConfig>,
) -> Result<(), DaemonError> {
  let mut stream = timeout(config.control_timeout, tls.accept(stream))
    .await
    .map_err(|_| DaemonError::ControlTimeout)??;
  handle_control(&mut stream, &config).await
}

async fn handle_control(
  stream: &mut TlsStream<TcpStream>,
  config: &DaemonConfig,
) -> Result<(), DaemonError> {
  let Some(hello) = receive_hello(stream, config.control_timeout).await? else {
    return Ok(());
  };

  let state = load(&config.state_dir)?;
  let mut challenge = [0_u8; 32];
  getrandom::fill(&mut challenge).map_err(|error| DaemonError::Random(error.to_string()))?;
  write_frame(
    stream,
    &ServerMessage::HelloAccepted {
      protocol_version: CONTROL_PROTOCOL_VERSION,
      server_version: env!("CARGO_PKG_VERSION").into(),
      device_id: state.device_id,
      challenge: challenge.to_vec(),
    },
  )
  .await?;

  match read_control(stream, config.control_timeout).await? {
    Some(ClientMessage::Pair {
      token,
      public_key,
      label,
    }) => handle_pair(stream, config, token, public_key, label).await?,
    Some(ClientMessage::Authenticate {
      public_key,
      signature,
    }) => handle_authentication(stream, config, public_key, signature, &challenge, &hello).await?,
    Some(_) => {
      send_error(
        stream,
        ErrorCode::AuthenticationRequired,
        "expected pairing or authentication".into(),
      )
      .await?;
    }
    None => {}
  }
  Ok(())
}

#[derive(Debug)]
struct ControlHello {
  client_name: String,
  client_version: String,
}

async fn receive_hello(
  stream: &mut TlsStream<TcpStream>,
  control_timeout: Duration,
) -> Result<Option<ControlHello>, DaemonError> {
  let Some(message) = read_control(stream, control_timeout).await? else {
    return Ok(None);
  };
  let ClientMessage::Hello {
    protocol_version,
    client_name,
    client_version,
  } = message
  else {
    send_error(stream, ErrorCode::InvalidRequest, "expected hello".into()).await?;
    return Ok(None);
  };
  if protocol_version != CONTROL_PROTOCOL_VERSION {
    send_error(
      stream,
      ErrorCode::ProtocolVersionMismatch,
      format!(
        "ctl protocol version {protocol_version} is unsupported; expected {CONTROL_PROTOCOL_VERSION}"
      ),
    )
    .await?;
    return Ok(None);
  }
  if !is_control_text(&client_name) || !is_control_text(&client_version) {
    send_error(
      stream,
      ErrorCode::InvalidRequest,
      "invalid client identity".into(),
    )
    .await?;
    return Ok(None);
  }
  Ok(Some(ControlHello {
    client_name,
    client_version,
  }))
}

async fn handle_pair(
  stream: &mut TlsStream<TcpStream>,
  config: &DaemonConfig,
  token: Vec<u8>,
  public_key: Vec<u8>,
  label: String,
) -> Result<(), DaemonError> {
  match consume_pairing(&config.state_dir, &token, &public_key, &label) {
    Ok(client) => {
      write_frame(
        stream,
        &ServerMessage::PairAccepted {
          client_id: client.client_id,
        },
      )
      .await?;
    }
    Err(StateError::PairingRejected | StateError::InvalidPublicKey | StateError::InvalidLabel) => {
      send_error(
        stream,
        ErrorCode::PairingRejected,
        "pairing was rejected".into(),
      )
      .await?;
    }
    Err(error) => return Err(error.into()),
  }
  Ok(())
}

async fn handle_authentication(
  stream: &mut TlsStream<TcpStream>,
  config: &DaemonConfig,
  public_key: Vec<u8>,
  signature: Vec<u8>,
  challenge: &[u8],
  hello: &ControlHello,
) -> Result<(), DaemonError> {
  let state = load(&config.state_dir)?;
  let Ok(client_id) = verify_authentication(
    &state,
    &public_key,
    &signature,
    challenge,
    &hello.client_name,
    &hello.client_version,
  ) else {
    send_error(
      stream,
      ErrorCode::AuthenticationFailed,
      "authentication was rejected".into(),
    )
    .await?;
    return Ok(());
  };
  write_frame(
    stream,
    &ServerMessage::Authenticated {
      client_id,
      capabilities: vec![Capability::RmuxTunnel],
    },
  )
  .await?;
  handle_open_service(stream, config).await
}

async fn handle_open_service(
  stream: &mut TlsStream<TcpStream>,
  config: &DaemonConfig,
) -> Result<(), DaemonError> {
  let Some(message) = read_control(stream, config.control_timeout).await? else {
    return Ok(());
  };
  let ClientMessage::OpenService {
    service: Service::Rmux,
    rmux_protocol_version,
  } = message
  else {
    send_error(
      stream,
      ErrorCode::InvalidRequest,
      "expected open_service".into(),
    )
    .await?;
    return Ok(());
  };
  if rmux_protocol_version != rmux_proto::PROTOCOL_VERSION {
    send_error(
      stream,
      ErrorCode::ProtocolVersionMismatch,
      format!(
        "rmux protocol version {rmux_protocol_version} is unsupported; expected {}",
        rmux_proto::PROTOCOL_VERSION
      ),
    )
    .await?;
    return Ok(());
  }
  let mut rmux = match connect_or_start_rmuxd(config).await {
    Ok(stream) => stream,
    Err(error) if is_rmux_unavailable(&error) => {
      send_error(
        stream,
        ErrorCode::Internal,
        "the local rmux service is unavailable".into(),
      )
      .await?;
      return Ok(());
    }
    Err(error) => return Err(error),
  };
  write_frame(
    stream,
    &ServerMessage::ServiceOpened {
      service: Service::Rmux,
    },
  )
  .await?;
  tokio::io::copy_bidirectional(stream, &mut rmux).await?;
  Ok(())
}

async fn read_control<S>(
  stream: &mut S,
  control_timeout: Duration,
) -> Result<Option<ClientMessage>, DaemonError>
where
  S: AsyncRead + Unpin,
{
  timeout(control_timeout, read_frame(stream))
    .await
    .map_err(|_| DaemonError::ControlTimeout)?
    .map_err(DaemonError::Codec)
}

async fn send_error<S>(stream: &mut S, code: ErrorCode, message: String) -> Result<(), DaemonError>
where
  S: AsyncWrite + Unpin,
{
  write_frame(stream, &ServerMessage::Error { code, message })
    .await
    .map_err(DaemonError::Codec)
}

fn verify_authentication(
  state: &DeviceState,
  public_key: &[u8],
  signature: &[u8],
  challenge: &[u8],
  client_name: &str,
  client_version: &str,
) -> Result<String, ()> {
  let public_key: [u8; 32] = public_key.try_into().map_err(|_| ())?;
  let signature: [u8; 64] = signature.try_into().map_err(|_| ())?;
  let client_id = client_id_from_public_key(&public_key);
  let authorized = state
    .authorized_clients
    .iter()
    .find(|client| client.client_id == client_id && !client.revoked)
    .ok_or(())?;
  if STANDARD
    .decode(&authorized.public_key_base64)
    .map_err(|_| ())?
    != public_key
  {
    return Err(());
  }
  let verifying_key = VerifyingKey::from_bytes(&public_key).map_err(|_| ())?;
  let signature = Signature::from_bytes(&signature);
  verifying_key
    .verify_strict(
      &authentication_payload(challenge, client_name, client_version),
      &signature,
    )
    .map_err(|_| ())?;
  Ok(client_id)
}

async fn connect_or_start_rmuxd(config: &DaemonConfig) -> Result<UnixStream, DaemonError> {
  match UnixStream::connect(&config.rmux_socket).await {
    Ok(stream) => return Ok(stream),
    Err(error)
      if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
      ) => {}
    Err(error) => return Err(DaemonError::RmuxConnect(error)),
  }

  let executable = config
    .rmuxd_bin
    .as_deref()
    .ok_or_else(|| DaemonError::RmuxUnavailable(config.rmux_socket.clone()))?;
  if !executable.is_absolute() {
    return Err(DaemonError::RelativeRmuXdPath(executable.to_path_buf()));
  }
  std::process::Command::new(executable)
    .arg("--socket")
    .arg(&config.rmux_socket)
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| DaemonError::StartRmuXd {
      executable: executable.to_path_buf(),
      source,
    })?;

  let deadline = Instant::now() + RMUX_START_TIMEOUT;
  loop {
    match UnixStream::connect(&config.rmux_socket).await {
      Ok(stream) => return Ok(stream),
      Err(error)
        if Instant::now() < deadline
          && matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
          ) =>
      {
        sleep(Duration::from_millis(25)).await;
      }
      Err(error) => return Err(DaemonError::RmuxConnect(error)),
    }
  }
}

fn is_control_text(value: &str) -> bool {
  !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn is_rmux_unavailable(error: &DaemonError) -> bool {
  matches!(
    error,
    DaemonError::RelativeRmuXdPath(_)
      | DaemonError::RmuxUnavailable(_)
      | DaemonError::RmuxConnect(_)
      | DaemonError::StartRmuXd { .. }
  )
}

#[derive(Debug, Error)]
pub enum DaemonError {
  #[error(
    "ctld refuses wildcard listen address {0}; choose an explicit Tailscale or loopback address"
  )]
  WildcardListenAddress(SocketAddr),
  #[error("the outer control handshake timed out")]
  ControlTimeout,
  #[error("secure randomness is unavailable: {0}")]
  Random(String),
  #[error("the configured rmuxd path must be absolute: {0}")]
  RelativeRmuXdPath(PathBuf),
  #[error("rmuxd is unavailable at {} and no companion rmuxd executable is configured", .0.display())]
  RmuxUnavailable(PathBuf),
  #[error("could not connect to rmuxd: {0}")]
  RmuxConnect(io::Error),
  #[error("could not start rmuxd using {}: {source}", executable.display())]
  StartRmuXd {
    executable: PathBuf,
    source: io::Error,
  },
  #[error(transparent)]
  State(#[from] StateError),
  #[error(transparent)]
  Base64(#[from] base64::DecodeError),
  #[error(transparent)]
  Codec(#[from] CodecError),
  #[error(transparent)]
  Io(#[from] io::Error),
  #[error("TLS configuration error: {0}")]
  TlsConfiguration(#[from] rustls::Error),
}
