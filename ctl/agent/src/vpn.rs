//! Restricted remote VPN requests; the full local broker is never exposed.
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ctl_ipc::remote_vpn::{PREFACE, Request, Response};
use ctl_ipc::{VpnSnapshot, VpnState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Serves the disposable process's SSH descriptors, preserving TCP half-close.
///
/// # Errors
/// Returns descriptor, framing, or relay I/O errors.
pub async fn serve_stdio(
  client: &ctl_ipc::vpn::Client,
  identity: &ctl_proto::RemoteIdentity,
) -> io::Result<()> {
  #[cfg(unix)]
  {
    let (mut reader, mut writer) = crate::stdio::take()?;
    serve(&mut reader, &mut writer, client, identity).await
  }
  #[cfg(not(unix))]
  serve(
    &mut tokio::io::stdin(),
    &mut tokio::io::stdout(),
    client,
    identity,
  )
  .await
}

/// Handles one VPN request through a fixed per-account local VPN client.
///
/// # Errors
/// Returns transport errors; operation failures use bounded structured frames.
pub async fn serve<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
  reader: &mut R,
  writer: &mut W,
  client: &ctl_ipc::vpn::Client,
  identity: &ctl_proto::RemoteIdentity,
) -> io::Result<()> {
  writer.write_all(PREFACE).await?;
  writer.flush().await?;
  tokio::time::timeout(
    Duration::from_secs(15),
    ctl_ipc::remote_vpn::accept_contract(reader, writer),
  )
  .await
  .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Remote VPN negotiation timed out"))?
  .map_err(io::Error::other)?;
  ctl_proto::write_identity(writer, identity).await?;
  writer.flush().await?;
  let request = tokio::time::timeout(
    Duration::from_secs(15),
    ctl_ipc::read_frame::<_, Request>(reader),
  )
  .await;
  let request = match request {
    Ok(Ok(Some(request))) => request,
    Ok(Ok(None)) => return Ok(()),
    Ok(Err(_)) => return failure(writer, "invalid_request", "Invalid remote VPN request").await,
    Err(_) => return failure(writer, "request_timeout", "Remote VPN request timed out").await,
  };
  if let Err(message) = request.validate() {
    return failure(writer, "invalid_request", &message).await;
  }
  match request {
    Request::List => match client.list().await {
      Ok(snapshot) => response(writer, &Response::Snapshot { snapshot }).await,
      Err(error) => vpn_failure(writer, error).await,
    },
    Request::Start { connection } => match client.start_connection(connection).await {
      Ok(status) => response(writer, &Response::Status { status }).await,
      Err(error) => vpn_failure(writer, error).await,
    },
    Request::Stop { vpn_id } => match client.stop_id(&vpn_id).await {
      Ok(status) => response(writer, &Response::Status { status }).await,
      Err(error) => vpn_failure(writer, error).await,
    },
    Request::Connect {
      connection_id,
      host,
      port,
    } => {
      let connected = tokio::time::timeout(
        Duration::from_secs(25),
        connect(client, &connection_id, &host, port),
      )
      .await;
      let mut stream = match connected {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
          return failure(writer, "vpn_connection_failed", &error.to_string()).await;
        }
        Err(_) => {
          return failure(
            writer,
            "vpn_connection_timeout",
            "Remote VPN connection timed out",
          )
          .await;
        }
      };
      response(writer, &Response::Connected).await?;
      let (mut upstream, mut downstream) = stream.split();
      let upload = async {
        tokio::io::copy(reader, &mut downstream).await?;
        downstream.shutdown().await
      };
      let download = async {
        tokio::io::copy(&mut upstream, writer).await?;
        writer.shutdown().await
      };
      tokio::try_join!(upload, download)?;
      Ok(())
    }
  }
}

async fn response(writer: &mut (impl AsyncWrite + Unpin), value: &Response) -> io::Result<()> {
  ctl_ipc::write_frame(writer, value)
    .await
    .map_err(io::Error::other)
}

async fn failure(
  writer: &mut (impl AsyncWrite + Unpin),
  code: &str,
  message: &str,
) -> io::Result<()> {
  response(
    writer,
    &Response::Error {
      code: code.into(),
      message: message.into(),
    },
  )
  .await
}

async fn vpn_failure(
  writer: &mut (impl AsyncWrite + Unpin),
  error: ctl_ipc::vpn::VpnError,
) -> io::Result<()> {
  match error {
    ctl_ipc::vpn::VpnError::Daemon { code, message } => failure(writer, &code, &message).await,
    error => failure(writer, "vpn_operation_failed", &error.to_string()).await,
  }
}

async fn connect(
  client: &ctl_ipc::vpn::Client,
  connection_id: &str,
  host: &str,
  port: u16,
) -> io::Result<tokio::net::TcpStream> {
  let snapshot = client.list().await.map_err(io::Error::other)?;
  let endpoint = connected_endpoint(&snapshot, connection_id)?;
  let mut stream = tokio::net::TcpStream::connect(endpoint).await?;
  socks_connect(&mut stream, host, port).await?;
  Ok(stream)
}

fn connected_endpoint(snapshot: &VpnSnapshot, connection_id: &str) -> io::Result<SocketAddr> {
  let mut matches = snapshot
    .connections
    .iter()
    .filter(|status| status.connection_id.as_deref() == Some(connection_id));
  let status = matches
    .next()
    .ok_or_else(|| io::Error::other("Selected VPN is not connected"))?;
  if matches.next().is_some()
    || !status.running
    || status.state != VpnState::Connected
    || status.status_unavailable
  {
    return Err(io::Error::other("Selected VPN is not connected"));
  }
  status
    .endpoint
    .as_deref()
    .and_then(|endpoint| endpoint.strip_prefix("socks5h://"))
    .and_then(|address| address.parse::<SocketAddr>().ok())
    .filter(|address| address.ip().is_loopback() && address.port() != 0)
    .ok_or_else(|| io::Error::other("Selected VPN has an invalid loopback SOCKS5 endpoint"))
}

async fn socks_connect(
  stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
  host: &str,
  port: u16,
) -> io::Result<()> {
  stream.write_all(&[5, 1, 0]).await?;
  let mut authentication = [0; 2];
  stream.read_exact(&mut authentication).await?;
  if authentication != [5, 0] {
    return Err(io::Error::other("VPN SOCKS5 authentication was rejected"));
  }
  let mut request = vec![5, 1, 0];
  match host.parse::<IpAddr>() {
    Ok(IpAddr::V4(ip)) => {
      request.push(1);
      request.extend_from_slice(&ip.octets());
    }
    Ok(IpAddr::V6(ip)) => {
      request.push(4);
      request.extend_from_slice(&ip.octets());
    }
    Err(_) => {
      request.push(3);
      request.push(
        u8::try_from(host.len()).map_err(|_| io::Error::other("Invalid SOCKS5 destination"))?,
      );
      request.extend_from_slice(host.as_bytes());
    }
  }
  request.extend_from_slice(&port.to_be_bytes());
  stream.write_all(&request).await?;
  let mut reply = [0; 4];
  stream.read_exact(&mut reply).await?;
  if reply[..3] != [5, 0, 0] {
    return Err(io::Error::other(
      "VPN SOCKS5 destination connection was rejected",
    ));
  }
  let address_bytes = match reply[3] {
    1 => 4,
    4 => 16,
    3 => usize::from(stream.read_u8().await?),
    _ => return Err(io::Error::other("Invalid VPN SOCKS5 reply")),
  };
  let mut remainder = vec![0; address_bytes + 2];
  stream.read_exact(&mut remainder).await?;
  Ok(())
}

#[cfg(test)]
mod tests;
