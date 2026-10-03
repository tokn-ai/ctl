//! OpenSSH `ProxyCommand` helper for ordered SSH, SOCKS5, and managed VPN routes.
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};

use ctl_ipc::{GatewayKind, SshGateway, VpnGateway, VpnState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use zeroize::Zeroizing;

trait ProxyStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ProxyStream for T {}
type Stream = Box<dyn ProxyStream>;

#[cfg(all(test, unix))]
mod vpn_tests;

struct ChildPipe {
  child: Child,
  stdin: Option<ChildStdin>,
  stdout: ChildStdout,
}

impl AsyncRead for ChildPipe {
  fn poll_read(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    Pin::new(&mut self.stdout).poll_read(context, buffer)
  }
}

impl AsyncWrite for ChildPipe {
  fn poll_write(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &[u8],
  ) -> Poll<io::Result<usize>> {
    match &mut self.stdin {
      Some(input) => Pin::new(input).poll_write(context, buffer),
      None => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
    }
  }
  fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    match &mut self.stdin {
      Some(input) => Pin::new(input).poll_flush(context),
      None => Poll::Ready(Ok(())),
    }
  }
  fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    if let Some(input) = &mut self.stdin {
      match Pin::new(input).poll_flush(context) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
        Poll::Ready(Ok(())) => {}
      }
    }
    self.stdin.take();
    Poll::Ready(Ok(()))
  }
}

impl Drop for ChildPipe {
  fn drop(&mut self) {
    let _ = self.child.start_kill();
  }
}

fn child_pipe(mut child: Child) -> io::Result<Stream> {
  let stdin = child
    .stdin
    .take()
    .ok_or_else(|| io::Error::other("proxy stdin missing"))?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| io::Error::other("proxy stdout missing"))?;
  Ok(Box::new(ChildPipe {
    child,
    stdin: Some(stdin),
    stdout,
  }))
}

fn host_port(host: &str, port: u16) -> String {
  if host.contains(':') && !host.starts_with('[') {
    format!("[{host}]:{port}")
  } else {
    format!("{host}:{port}")
  }
}

async fn connect(gateways: &[SshGateway], host: &str, port: u16) -> io::Result<Stream> {
  let Some((gateway, prefix)) = gateways.split_last() else {
    return Ok(Box::new(
      tokio::net::TcpStream::connect(host_port(host, port)).await?,
    ));
  };
  if super::invalid_gateway(gateway) {
    return Err(io::Error::other("invalid proxy gateway"));
  }
  let endpoint = gateway.hostname.as_deref().unwrap_or(&gateway.destination);
  match gateway.kind {
    GatewayKind::Ssh => {
      let mut command = Command::new("ssh");
      command
        .args(["-T", "-W", &host_port(host, port)])
        .args(["-o", "ControlPath=none"])
        .args(["-o", "ControlMaster=no"])
        .args(["-o", "ControlPersist=no"])
        .args(["-o", "ForkAfterAuthentication=no"])
        .args(["-o", "StdinNull=no"])
        .args(["-o", "ClearAllForwardings=yes"])
        .args(["-o", "ForwardAgent=no"])
        .args(["-o", "ForwardX11=no"])
        .args(["-o", "PermitLocalCommand=no"])
        .args(["-o", "RemoteCommand=none"]);
      if !prefix.is_empty() {
        let executable = std::env::current_exe()?;
        let proxy = ctl_ipc::proxy_command_with_executable(prefix, &executable);
        command.arg("-o").arg(format!("ProxyCommand={proxy}"));
      }
      if let Some(port) = gateway.port {
        command.args(["-p", &port.to_string()]);
      }
      if let Some(user) = &gateway.user {
        command.args(["-l", user]);
      }
      if let Some(hostname) = &gateway.hostname {
        command.arg("-o").arg(format!("HostName={hostname}"));
      }
      command
        .arg("--")
        .arg(&gateway.destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
      child_pipe(command.spawn()?)
    }
    GatewayKind::Socks5 => {
      let mut stream = Box::pin(connect(prefix, endpoint, gateway.port.unwrap_or(1080))).await?;
      socks_connect(&mut stream, gateway, host, port).await?;
      Ok(stream)
    }
    GatewayKind::Vpn => {
      let vpn = gateway
        .vpn
        .as_ref()
        .ok_or_else(|| io::Error::other("VPN gateway reference is missing"))?;
      if !prefix.is_empty() {
        let owner = ctl_ipc::vpn_owner_target(gateways, gateways.len() - 1)
          .ok_or_else(|| io::Error::other("VPN must be first or follow an SSH host"))?;
        let client = ctl_ipc::remote_vpn::Client::new(owner, vpn.expected_remote_id.clone())
          .with_proxy_executable(std::env::current_exe()?);
        return Ok(Box::new(
          client
            .open_connection(&vpn.connection_id, host, port)
            .await
            .map_err(io::Error::other)?,
        ));
      }
      let endpoint = vpn_endpoint(vpn).await?;
      let mut stream: Stream = Box::new(tokio::net::TcpStream::connect(endpoint).await?);
      socks_connect(&mut stream, gateway, host, port).await?;
      Ok(stream)
    }
  }
}

async fn vpn_endpoint(vpn: &VpnGateway) -> io::Result<SocketAddr> {
  let snapshot = ctl_ipc::vpn::Client::new(vpn.socket_path.clone())
    .list()
    .await
    .map_err(io::Error::other)?;
  let mut matches = snapshot
    .connections
    .iter()
    .filter(|status| status.connection_id.as_deref() == Some(&vpn.connection_id));
  let status = matches
    .next()
    .ok_or_else(|| io::Error::other("selected VPN is not connected"))?;
  if matches.next().is_some()
    || !status.running
    || status.state != VpnState::Connected
    || status.status_unavailable
  {
    return Err(io::Error::other("selected VPN is not connected"));
  }
  let endpoint = status
    .endpoint
    .as_deref()
    .ok_or_else(|| io::Error::other("selected VPN has no SOCKS5 endpoint"))?;
  parse_vpn_endpoint(endpoint)
}

fn parse_vpn_endpoint(endpoint: &str) -> io::Result<SocketAddr> {
  // Parsing a numeric socket address also rejects DNS names, credentials,
  // paths, queries, and fragments. The managed listener is always local.
  endpoint
    .strip_prefix("socks5h://")
    .and_then(|address| address.parse::<SocketAddr>().ok())
    .filter(|address| address.ip().is_loopback() && address.port() != 0)
    .ok_or_else(|| io::Error::other("selected VPN has an invalid loopback SOCKS5 endpoint"))
}

async fn socks_connect(
  stream: &mut Stream,
  gateway: &SshGateway,
  host: &str,
  port: u16,
) -> io::Result<()> {
  let authenticated = gateway.user.is_some();
  stream
    .write_all(if authenticated {
      &[5, 1, 2]
    } else {
      &[5, 1, 0]
    })
    .await?;
  let mut reply = [0; 2];
  stream.read_exact(&mut reply).await?;
  if reply[0] != 5 || reply[1] != if authenticated { 2 } else { 0 } {
    return Err(io::Error::other(
      "SOCKS5 proxy rejected the authentication method",
    ));
  }
  if let Some(user) = &gateway.user {
    let password = askpass(gateway).await?;
    if user.len() > 255 || password.len() > 255 {
      return Err(io::Error::other("SOCKS5 credentials exceed 255 bytes"));
    }
    let user_len =
      u8::try_from(user.len()).map_err(|_| io::Error::other("SOCKS5 username is too long"))?;
    let password_len =
      u8::try_from(password.len()).map_err(|_| io::Error::other("SOCKS5 password is too long"))?;
    let mut request = vec![1, user_len];
    request.extend_from_slice(user.as_bytes());
    request.push(password_len);
    request.extend_from_slice(password.as_bytes());
    stream.write_all(&request).await?;
    request.fill(0);
    stream.read_exact(&mut reply).await?;
    if reply != [1, 0] {
      return Err(io::Error::other("SOCKS5 authentication failed"));
    }
  }
  let address = if let Ok(ip) = host.parse::<IpAddr>() {
    match ip {
      IpAddr::V4(ip) => {
        let mut bytes = vec![1];
        bytes.extend_from_slice(&ip.octets());
        bytes
      }
      IpAddr::V6(ip) => {
        let mut bytes = vec![4];
        bytes.extend_from_slice(&ip.octets());
        bytes
      }
    }
  } else {
    if host.is_empty() || host.len() > 255 {
      return Err(io::Error::other("invalid SOCKS5 destination"));
    }
    let host_len =
      u8::try_from(host.len()).map_err(|_| io::Error::other("SOCKS5 destination is too long"))?;
    let mut bytes = vec![3, host_len];
    bytes.extend_from_slice(host.as_bytes());
    bytes
  };
  let mut request = vec![5, 1, 0];
  request.extend_from_slice(&address);
  request.extend_from_slice(&port.to_be_bytes());
  stream.write_all(&request).await?;
  let mut header = [0; 4];
  stream.read_exact(&mut header).await?;
  if header[0] != 5 || header[1] != 0 {
    return Err(io::Error::other(format!(
      "SOCKS5 connect failed: code {}",
      header[1]
    )));
  }
  let address_len = match header[3] {
    1 => 4,
    4 => std::mem::size_of::<Ipv6Addr>(),
    3 => {
      let mut len = [0];
      stream.read_exact(&mut len).await?;
      usize::from(len[0])
    }
    _ => return Err(io::Error::other("invalid SOCKS5 response address")),
  };
  let mut discard = vec![0; address_len + 2];
  stream.read_exact(&mut discard).await?;
  Ok(())
}

async fn askpass(gateway: &SshGateway) -> io::Result<Zeroizing<String>> {
  let user = gateway.user.as_deref().unwrap_or_default();
  let message = format!("SOCKS5 {user}@{} password:", gateway.destination);
  let Some(program) = std::env::var_os("SSH_ASKPASS") else {
    return tokio::task::spawn_blocking(move || rpassword::prompt_password(message))
      .await
      .map_err(io::Error::other)?
      .map(Zeroizing::new);
  };
  let output = Command::new(program)
    .arg(message)
    .stdin(Stdio::null())
    .stderr(Stdio::null())
    .output()
    .await?;
  if !output.status.success() || output.stdout.len() > 256 {
    return Err(io::Error::other("SOCKS5 password prompt was cancelled"));
  }
  Ok(Zeroizing::new(
    String::from_utf8(output.stdout)
      .map_err(io::Error::other)?
      .trim_end_matches(['\r', '\n'])
      .to_owned(),
  ))
}

/// Runs a proxy route from process stdin to stdout.
///
/// # Errors
/// Returns an error for an invalid route, connection failure, or interrupted relay.
pub async fn run(route: &str, host: &str, port: u16) -> io::Result<()> {
  let (pairs, remainder) = route.as_bytes().as_chunks::<2>();
  if !remainder.is_empty() {
    return Err(io::Error::other("invalid proxy route encoding"));
  }
  let bytes = pairs
    .iter()
    .map(|pair| {
      let text = std::str::from_utf8(pair).map_err(io::Error::other)?;
      u8::from_str_radix(text, 16).map_err(io::Error::other)
    })
    .collect::<io::Result<Vec<_>>>()?;
  let gateways: Vec<SshGateway> = serde_json::from_slice(&bytes)?;
  if !ctl_ipc::has_valid_gateway_route(&gateways)
    || gateways.iter().any(super::invalid_gateway)
    || port == 0
    || host.is_empty()
  {
    return Err(io::Error::other("invalid proxy route"));
  }
  let mut stream = connect(&gateways, host, port).await?;
  let mut stdio = StdioPipe {
    input: tokio::io::stdin(),
    output: tokio::io::stdout(),
  };
  tokio::io::copy_bidirectional(&mut stdio, &mut stream).await?;
  Ok(())
}

struct StdioPipe {
  input: tokio::io::Stdin,
  output: tokio::io::Stdout,
}
impl AsyncRead for StdioPipe {
  fn poll_read(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    Pin::new(&mut self.input).poll_read(context, buffer)
  }
}
impl AsyncWrite for StdioPipe {
  fn poll_write(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &[u8],
  ) -> Poll<io::Result<usize>> {
    Pin::new(&mut self.output).poll_write(context, buffer)
  }
  fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.output).poll_flush(context)
  }
  fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.output).poll_shutdown(context)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctl_ipc::SshGatewayMode;
  use tokio::net::TcpListener;

  #[tokio::test]
  async fn two_socks5_hops_preserve_route_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
      let (mut first, _) = listener.accept().await.unwrap();
      let first_hop = tokio::spawn(async move {
        let mut greeting = [0; 3];
        first.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        first.write_all(&[5, 0]).await.unwrap();
        let mut request = [0; 10];
        first.read_exact(&mut request).await.unwrap();
        assert_eq!(&request[..8], &[5, 1, 0, 1, 127, 0, 0, 1]);
        assert_eq!(u16::from_be_bytes([request[8], request[9]]), proxy_port);
        let mut upstream = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
          .await
          .unwrap();
        first
          .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
          .await
          .unwrap();
        tokio::io::copy_bidirectional(&mut first, &mut upstream)
          .await
          .unwrap();
      });
      let (mut second, _) = listener.accept().await.unwrap();
      let mut greeting = [0; 3];
      second.read_exact(&mut greeting).await.unwrap();
      assert_eq!(greeting, [5, 1, 0]);
      second.write_all(&[5, 0]).await.unwrap();
      let mut request = [0; 5];
      second.read_exact(&mut request).await.unwrap();
      assert_eq!(request, [5, 1, 0, 3, 15]);
      let mut rest = [0; 17];
      second.read_exact(&mut rest).await.unwrap();
      assert_eq!(&rest[..15], b"target.internal");
      assert_eq!(u16::from_be_bytes([rest[15], rest[16]]), 2222);
      second
        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
        .await
        .unwrap();
      second.write_all(b"ok").await.unwrap();
      drop(second);
      first_hop.await.unwrap();
    });
    let gateway = SshGateway {
      kind: GatewayKind::Socks5,
      vpn: None,
      destination: "127.0.0.1".into(),
      hostname: None,
      user: None,
      port: Some(proxy_port),
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    };
    let mut connection = connect(&[gateway.clone(), gateway], "target.internal", 2222)
      .await
      .unwrap();
    let mut response = [0; 2];
    connection.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"ok");
    drop(connection);
    server.await.unwrap();
  }

  #[tokio::test]
  async fn socks5_passes_destination_name_to_proxy_and_relays_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      let mut greeting = [0; 3];
      stream.read_exact(&mut greeting).await.unwrap();
      assert_eq!(greeting, [5, 1, 0]);
      stream.write_all(&[5, 0]).await.unwrap();
      let mut request = [0; 5];
      stream.read_exact(&mut request).await.unwrap();
      assert_eq!(request, [5, 1, 0, 3, 15]);
      let mut name = [0; 15];
      stream.read_exact(&mut name).await.unwrap();
      assert_eq!(&name, b"target.internal");
      let mut port = [0; 2];
      stream.read_exact(&mut port).await.unwrap();
      assert_eq!(u16::from_be_bytes(port), 2222);
      stream
        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
        .await
        .unwrap();
      let mut payload = [0; 4];
      stream.read_exact(&mut payload).await.unwrap();
      assert_eq!(&payload, b"ping");
      stream.write_all(b"pong").await.unwrap();
    });
    let gateway = SshGateway {
      kind: GatewayKind::Socks5,
      vpn: None,
      destination: "127.0.0.1".into(),
      hostname: None,
      user: None,
      port: Some(port),
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    };
    let mut connection = connect(&[gateway], "target.internal", 2222).await.unwrap();
    connection.write_all(b"ping").await.unwrap();
    let mut response = [0; 4];
    connection.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"pong");
    server.await.unwrap();
  }
}
