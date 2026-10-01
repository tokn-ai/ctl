//! Bounded SSH greeting checks that never authenticate or create a connection master.

mod config;

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ctl_ipc::{GatewayKind, SshGateway, SshTarget, VpnGateway, VpnState};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ROUTE_HOPS: usize = 8;
const MAX_GREETING_BYTES: usize = 8192;
const MAX_IDENTIFICATION_BYTES: usize = 255;
static PROBE_LIMIT: Semaphore = Semaphore::const_new(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshReachabilityState {
  Available,
  Unavailable,
  NotChecked,
  Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshReachabilityReason {
  VpnDisconnected,
  RouteRequiresConnection,
  UnsupportedConfiguration,
  ConnectionRefused,
  TimedOut,
  InvalidGreeting,
  CheckFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshReachability {
  pub state: SshReachabilityState,
  pub reason: Option<SshReachabilityReason>,
  pub message: Option<String>,
}

impl SshReachability {
  fn new(
    state: SshReachabilityState,
    reason: SshReachabilityReason,
    message: impl Into<String>,
  ) -> Self {
    Self {
      state,
      reason: Some(reason),
      message: Some(message.into()),
    }
  }

  fn available() -> Self {
    Self {
      state: SshReachabilityState::Available,
      reason: None,
      message: None,
    }
  }
}

/// Reads an SSH service greeting over the configured route, then closes it.
/// This never authenticates, runs SSH commands, starts a daemon or VPN, or
/// falls back to a direct route when a configured route cannot be checked.
pub async fn probe(target: &SshTarget) -> SshReachability {
  let checking_service = AtomicBool::new(false);
  within_deadline(PROBE_TIMEOUT, &checking_service, async {
    // The queue is part of the deadline, so a large catalog cannot leave a
    // backlog of obsolete probes opening sockets after their callers give up.
    let Ok(_permit) = PROBE_LIMIT.acquire().await else {
      return check_failed("SSH availability checks are unavailable.");
    };
    if let Err(result) = preflight_route(&target.gateways) {
      return result;
    }
    let endpoint = match config::resolve(target).await {
      Ok(endpoint) => endpoint,
      Err(result) => return result,
    };
    probe_endpoint(&target.gateways, &endpoint, &checking_service).await
  })
  .await
}

async fn within_deadline(
  deadline: Duration,
  checking_service: &AtomicBool,
  operation: impl Future<Output = SshReachability>,
) -> SshReachability {
  tokio::time::timeout(deadline, operation)
    .await
    .unwrap_or_else(|_| {
      SshReachability::new(
        if checking_service.load(Ordering::Relaxed) {
          SshReachabilityState::Unavailable
        } else {
          SshReachabilityState::Unknown
        },
        SshReachabilityReason::TimedOut,
        "The SSH availability check timed out.",
      )
    })
}

fn preflight_route(gateways: &[SshGateway]) -> Result<(), SshReachability> {
  if gateways.len() > MAX_ROUTE_HOPS {
    return Err(unsupported(
      "The configured route has too many gateways to check.",
    ));
  }
  for (index, gateway) in gateways.iter().enumerate() {
    if !gateway.has_valid_vpn_configuration() {
      return Err(unsupported("The configured VPN route is invalid."));
    }
    match gateway.kind {
      GatewayKind::Ssh => {
        return Err(route_requires_connection(
          "Checking this route requires an SSH gateway connection.",
        ));
      }
      GatewayKind::Socks5 => {
        if gateway.user.is_some() {
          return Err(route_requires_connection(
            "Checking this SOCKS5 route requires authentication.",
          ));
        }
        let host = gateway.hostname.as_deref().unwrap_or(&gateway.destination);
        if !valid_host(host) || gateway.port == Some(0) {
          return Err(unsupported("The configured SOCKS5 endpoint is invalid."));
        }
      }
      GatewayKind::Vpn if index != 0 => {
        return Err(unsupported(
          "A managed VPN must be the first gateway in the route.",
        ));
      }
      GatewayKind::Vpn => {}
    }
  }
  Ok(())
}

async fn probe_endpoint(
  gateways: &[SshGateway],
  endpoint: &config::Endpoint,
  checking_service: &AtomicBool,
) -> SshReachability {
  let mut stream = match connect_route(gateways, endpoint, checking_service).await {
    Ok(stream) => stream,
    Err(result) => return result,
  };
  // SSH servers send identification before key exchange. Reading without
  // sending our own identification cannot advance to authentication.
  match read_greeting(&mut BufReader::new(&mut stream)).await {
    Ok(()) => SshReachability::available(),
    Err(result) => result,
  }
}

async fn connect_route(
  gateways: &[SshGateway],
  endpoint: &config::Endpoint,
  checking_service: &AtomicBool,
) -> Result<TcpStream, SshReachability> {
  let Some(first) = gateways.first() else {
    // DNS failure is inconclusive, and resolution timeout is not evidence
    // that the remote SSH service refused a connection.
    let addresses = tokio::net::lookup_host((endpoint.host.as_str(), endpoint.port))
      .await
      .map_err(|error| check_failed(format!("Could not resolve the SSH endpoint: {error}")))?
      .collect::<Vec<_>>();
    if addresses.is_empty() {
      return Err(check_failed(
        "The SSH endpoint did not resolve to an address.",
      ));
    }
    checking_service.store(true, Ordering::Relaxed);
    return TcpStream::connect(addresses.as_slice())
      .await
      .map_err(network_error);
  };
  let mut stream = match first.kind {
    GatewayKind::Vpn => {
      let vpn = first
        .vpn
        .as_ref()
        .ok_or_else(|| unsupported("The configured VPN reference is missing."))?;
      let address = vpn_endpoint(vpn).await?;
      TcpStream::connect(address).await.map_err(proxy_error)?
    }
    GatewayKind::Socks5 => {
      let host = first.hostname.as_deref().unwrap_or(&first.destination);
      TcpStream::connect((host, first.port.unwrap_or(1080)))
        .await
        .map_err(proxy_error)?
    }
    GatewayKind::Ssh => {
      return Err(route_requires_connection(
        "Checking this route requires an SSH gateway connection.",
      ));
    }
  };
  for gateway in gateways.iter().skip(1) {
    let host = gateway.hostname.as_deref().unwrap_or(&gateway.destination);
    socks_connect(
      &mut stream,
      host,
      gateway.port.unwrap_or(1080),
      false,
      checking_service,
    )
    .await?;
  }
  socks_connect(
    &mut stream,
    &endpoint.host,
    endpoint.port,
    true,
    checking_service,
  )
  .await?;
  Ok(stream)
}

async fn vpn_endpoint(vpn: &VpnGateway) -> Result<SocketAddr, SshReachability> {
  // list() is pinned to this owner and never starts a daemon or VPN.
  let snapshot = ctl_ipc::vpn::Client::new(vpn.socket_path.clone())
    .list()
    .await
    .map_err(|error| check_failed(format!("Could not check the selected VPN: {error}")))?;
  connected_vpn_endpoint(&snapshot, &vpn.connection_id)
}

fn connected_vpn_endpoint(
  snapshot: &ctl_ipc::VpnSnapshot,
  connection_id: &str,
) -> Result<SocketAddr, SshReachability> {
  let mut matches = snapshot
    .connections
    .iter()
    .filter(|status| status.connection_id.as_deref() == Some(connection_id));
  let Some(status) = matches.next() else {
    return Err(vpn_disconnected());
  };
  if matches.next().is_some() {
    return Err(check_failed(
      "The selected VPN has an ambiguous running connection.",
    ));
  }
  if !status.running || status.state != VpnState::Connected {
    return Err(vpn_disconnected());
  }
  status
    .endpoint
    .as_deref()
    .and_then(|endpoint| endpoint.strip_prefix("socks5h://"))
    .and_then(|address| address.parse::<SocketAddr>().ok())
    .filter(|address| address.ip().is_loopback() && address.port() != 0)
    .ok_or_else(|| check_failed("The selected VPN has no valid local SOCKS5 endpoint."))
}

async fn socks_connect(
  stream: &mut TcpStream,
  host: &str,
  port: u16,
  final_hop: bool,
  checking_service: &AtomicBool,
) -> Result<(), SshReachability> {
  // Offer no authentication only: this check must never access credentials.
  stream.write_all(&[5, 1, 0]).await.map_err(proxy_error)?;
  let mut negotiation = [0; 2];
  stream
    .read_exact(&mut negotiation)
    .await
    .map_err(proxy_error)?;
  if negotiation != [5, 0] {
    return Err(route_requires_connection(
      "The SOCKS5 proxy did not accept a check without authentication.",
    ));
  }
  let mut request = vec![5, 1, 0];
  match host.parse::<IpAddr>() {
    Ok(IpAddr::V4(address)) => {
      request.push(1);
      request.extend_from_slice(&address.octets());
    }
    Ok(IpAddr::V6(address)) => {
      request.push(4);
      request.extend_from_slice(&address.octets());
    }
    Err(_) => {
      let length = u8::try_from(host.len())
        .map_err(|_| unsupported("The SSH hostname is too long for its SOCKS5 route."))?;
      if !valid_host(host) {
        return Err(unsupported("The SSH hostname is invalid."));
      }
      // Preserve DNS resolution through the selected proxy/VPN.
      request.extend_from_slice(&[3, length]);
      request.extend_from_slice(host.as_bytes());
    }
  }
  request.extend_from_slice(&port.to_be_bytes());
  stream.write_all(&request).await.map_err(proxy_error)?;
  checking_service.store(final_hop, Ordering::Relaxed);
  let mut header = [0; 4];
  stream.read_exact(&mut header).await.map_err(proxy_error)?;
  if header[0] != 5 || header[2] != 0 {
    return Err(check_failed(
      "The SOCKS5 proxy returned an invalid response.",
    ));
  }
  if header[1] != 0 {
    return Err(SshReachability::new(
      if final_hop && matches!(header[1], 3..=6) {
        SshReachabilityState::Unavailable
      } else {
        SshReachabilityState::Unknown
      },
      match header[1] {
        5 => SshReachabilityReason::ConnectionRefused,
        6 => SshReachabilityReason::TimedOut,
        _ => SshReachabilityReason::CheckFailed,
      },
      "The SOCKS5 proxy could not reach the SSH service.",
    ));
  }
  let address_length = match header[3] {
    1 => 4,
    4 => 16,
    3 => usize::from(stream.read_u8().await.map_err(proxy_error)?),
    _ => {
      return Err(check_failed(
        "The SOCKS5 proxy returned an invalid address.",
      ));
    }
  };
  let mut reply = vec![0; address_length + 2];
  stream.read_exact(&mut reply).await.map_err(proxy_error)?;
  Ok(())
}

async fn read_greeting(reader: &mut (impl AsyncRead + Unpin)) -> Result<(), SshReachability> {
  let mut line = Vec::new();
  for _ in 0..MAX_GREETING_BYTES {
    let byte = match reader.read_u8().await {
      Ok(byte) => byte,
      Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Err(invalid_greeting()),
      Err(error) => return Err(network_error(error)),
    };
    line.push(byte);
    if byte != b'\n' {
      continue;
    }
    if line.starts_with(b"SSH-") {
      return if valid_identification(&line) {
        Ok(())
      } else {
        Err(invalid_greeting())
      };
    }
    // RFC 4253 permits text lines before the SSH identification string.
    // Reject binary service responses without retaining arbitrary output.
    if std::str::from_utf8(&line).is_err()
      || line
        .iter()
        .any(|byte| byte.is_ascii_control() && !b"\t\r\n".contains(byte))
    {
      return Err(invalid_greeting());
    }
    line.clear();
  }
  Err(invalid_greeting())
}

fn valid_identification(line: &[u8]) -> bool {
  if line.len() > MAX_IDENTIFICATION_BYTES {
    return false;
  }
  let line = line.strip_suffix(b"\n").unwrap_or(line);
  let line = line.strip_suffix(b"\r").unwrap_or(line);
  let Some(version) = line
    .strip_prefix(b"SSH-2.0-")
    .or_else(|| line.strip_prefix(b"SSH-1.99-"))
  else {
    return false;
  };
  let software = version
    .split(|byte| *byte == b' ')
    .next()
    .unwrap_or_default();
  !software.is_empty()
    && software
      .iter()
      .all(|byte| byte.is_ascii_graphic() && *byte != b'-')
    && version
      .iter()
      .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
}

fn valid_host(host: &str) -> bool {
  !host.is_empty()
    && host.len() <= 255
    && !host.chars().any(|ch| ch.is_control() || ch.is_whitespace())
}

// Result::map_err passes owned I/O errors to these conversion functions.
#[allow(clippy::needless_pass_by_value)]
fn network_error(error: io::Error) -> SshReachability {
  let reason = match error.kind() {
    io::ErrorKind::ConnectionRefused => SshReachabilityReason::ConnectionRefused,
    io::ErrorKind::TimedOut => SshReachabilityReason::TimedOut,
    _ => SshReachabilityReason::CheckFailed,
  };
  let state = match error.kind() {
    io::ErrorKind::ConnectionRefused
    | io::ErrorKind::TimedOut
    | io::ErrorKind::ConnectionReset
    | io::ErrorKind::ConnectionAborted
    | io::ErrorKind::HostUnreachable
    | io::ErrorKind::NetworkUnreachable => SshReachabilityState::Unavailable,
    _ => SshReachabilityState::Unknown,
  };
  SshReachability::new(
    state,
    reason,
    format!("Could not reach the SSH service: {error}"),
  )
}

#[allow(clippy::needless_pass_by_value)]
fn proxy_error(error: io::Error) -> SshReachability {
  check_failed(format!(
    "Could not check the selected SOCKS5 route: {error}"
  ))
}

fn invalid_greeting() -> SshReachability {
  SshReachability::new(
    SshReachabilityState::Unavailable,
    SshReachabilityReason::InvalidGreeting,
    "The endpoint did not provide a valid SSH service greeting.",
  )
}

fn check_failed(message: impl Into<String>) -> SshReachability {
  SshReachability::new(
    SshReachabilityState::Unknown,
    SshReachabilityReason::CheckFailed,
    message,
  )
}

fn unsupported(message: impl Into<String>) -> SshReachability {
  SshReachability::new(
    SshReachabilityState::NotChecked,
    SshReachabilityReason::UnsupportedConfiguration,
    message,
  )
}

fn route_requires_connection(message: impl Into<String>) -> SshReachability {
  SshReachability::new(
    SshReachabilityState::NotChecked,
    SshReachabilityReason::RouteRequiresConnection,
    message,
  )
}

fn vpn_disconnected() -> SshReachability {
  SshReachability::new(
    SshReachabilityState::NotChecked,
    SshReachabilityReason::VpnDisconnected,
    "The selected VPN is disconnected. Connect it to check SSH availability.",
  )
}

#[cfg(test)]
mod tests;
