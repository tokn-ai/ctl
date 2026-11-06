//! A disposable SSH process owns one connected remote UDP socket.

use std::io;
use std::net::SocketAddr;

use ctl_proto::forwarding::{self, MAX_DATAGRAM_BYTES, Request, Response};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio::net::UdpSocket;

/// Serves this process's SSH descriptors without retaining them after EOF.
///
/// # Errors
/// Returns descriptor, setup, framing, or UDP transport errors.
pub async fn serve_stdio() -> io::Result<()> {
  #[cfg(unix)]
  {
    let (mut reader, mut writer) = ctl_ipc::stdio::take()?;
    serve(&mut reader, &mut writer).await
  }
  #[cfg(not(unix))]
  serve(&mut tokio::io::stdin(), &mut tokio::io::stdout()).await
}

/// Negotiates a forwarding contract, validates a destination, and relays UDP.
///
/// # Errors
/// Returns framing and I/O errors. Destination failures use a structured setup
/// response before any datagram frames are exchanged.
pub async fn serve(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<()> {
  let socket = tokio::time::timeout(forwarding::STARTUP_TIMEOUT, async {
    writer.write_all(forwarding::PREFACE).await?;
    writer.flush().await?;
    forwarding::accept_contract(reader, writer).await?;
    let request: Request = forwarding::read(reader).await?;
    if let Err(error) = request.validate() {
      forwarding::write(
        writer,
        &Response::Error {
          message: error.to_string(),
        },
      )
      .await?;
      return Ok::<_, io::Error>(None);
    }
    match connect(&request).await {
      Ok(socket) => {
        forwarding::write(writer, &Response::Ready).await?;
        Ok(Some(socket))
      }
      Err(error) => {
        forwarding::write(
          writer,
          &Response::Error {
            message: error.to_string(),
          },
        )
        .await?;
        Ok(None)
      }
    }
  })
  .await
  .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UDP forwarding setup timed out"))??;
  let Some(socket) = socket else {
    return Ok(());
  };
  relay(reader, writer, &socket).await
}

async fn connect(request: &Request) -> io::Result<UdpSocket> {
  let addresses = tokio::net::lookup_host((request.host.as_str(), request.port)).await?;
  let mut last_error = None;
  for address in addresses {
    match connect_address(address).await {
      Ok(socket) => return Ok(socket),
      Err(error) => last_error = Some(error),
    }
  }
  Err(last_error.unwrap_or_else(|| io::Error::other("UDP destination resolved to no addresses")))
}

async fn connect_address(address: SocketAddr) -> io::Result<UdpSocket> {
  let bind = if address.is_ipv6() {
    "[::]:0"
  } else {
    "0.0.0.0:0"
  };
  let socket = UdpSocket::bind(bind).await?;
  socket.connect(address).await?;
  Ok(socket)
}

async fn relay(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
  socket: &UdpSocket,
) -> io::Result<()> {
  let upload = async {
    while let Some(bytes) = forwarding::read_datagram(reader).await? {
      socket.send(&bytes).await?;
    }
    Ok::<_, io::Error>(())
  };
  let download = async {
    let mut buffer = vec![0; MAX_DATAGRAM_BYTES + 1];
    loop {
      let count = socket.recv(&mut buffer).await?;
      forwarding::write_datagram(writer, &buffer[..count]).await?;
    }
    #[allow(unreachable_code)]
    Ok::<_, io::Error>(())
  };
  // Keep both complete loops alive: cancelling an individual framed read when
  // a UDP reply arrives could lose a partially consumed header or payload.
  tokio::select! {
    result = upload => result,
    result = download => result,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;
  use tokio::io::AsyncReadExt as _;

  async fn open(
    stream: tokio::io::DuplexStream,
    port: u16,
  ) -> (
    tokio::io::ReadHalf<tokio::io::DuplexStream>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
  ) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut preface = vec![0; forwarding::PREFACE.len()];
    reader.read_exact(&mut preface).await.unwrap();
    assert_eq!(preface, forwarding::PREFACE);
    forwarding::negotiate_contract(&mut reader, &mut writer)
      .await
      .unwrap();
    forwarding::write(
      &mut writer,
      &Request {
        host: "127.0.0.1".into(),
        port,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      forwarding::read::<Response>(&mut reader).await.unwrap(),
      Response::Ready
    ));
    (reader, writer)
  }

  #[tokio::test]
  async fn relays_empty_binary_and_large_datagrams_and_stops_on_eof() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let echo = tokio::spawn(async move {
      let mut buffer = vec![0; MAX_DATAGRAM_BYTES];
      for _ in 0..3 {
        let (count, address) = echo.recv_from(&mut buffer).await.unwrap();
        echo.send_to(&buffer[..count], address).await.unwrap();
      }
    });
    let (client, server) = tokio::io::duplex(2 * MAX_DATAGRAM_BYTES);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = open(client, port).await;
    // Fits macOS's default UDP send limit; maximum framing is tested in proto.
    for bytes in [vec![], vec![0, 255, 13, 10], vec![123; 8000]] {
      forwarding::write_datagram(&mut writer, &bytes)
        .await
        .unwrap();
      assert_eq!(
        tokio::time::timeout(
          Duration::from_secs(2),
          forwarding::read_datagram(&mut reader)
        )
        .await
        .unwrap()
        .unwrap(),
        Some(bytes)
      );
    }
    writer.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
      .await
      .unwrap()
      .unwrap();
    echo.await.unwrap();
  }

  #[tokio::test]
  async fn two_channels_keep_udp_peers_independent() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let mut clients = Vec::new();
    let mut servers = Vec::new();
    for byte in [1, 2] {
      let (client, server) = tokio::io::duplex(4096);
      servers.push(tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(server);
        serve(&mut reader, &mut writer).await.unwrap();
      }));
      let (reader, mut writer) = open(client, port).await;
      forwarding::write_datagram(&mut writer, &[byte])
        .await
        .unwrap();
      clients.push((reader, writer));
    }
    let mut buffer = [0; 16];
    let (count, first) = echo.recv_from(&mut buffer).await.unwrap();
    assert_eq!(&buffer[..count], &[1]);
    let (count, second) = echo.recv_from(&mut buffer).await.unwrap();
    assert_eq!(&buffer[..count], &[2]);
    assert_ne!(first, second);
    echo.send_to(&[2], second).await.unwrap();
    echo.send_to(&[1], first).await.unwrap();
    for (index, (reader, writer)) in clients.iter_mut().enumerate() {
      assert_eq!(
        forwarding::read_datagram(reader).await.unwrap(),
        Some(vec![u8::try_from(index + 1).unwrap()])
      );
      writer.shutdown().await.unwrap();
    }
    for server in servers {
      server.await.unwrap();
    }
  }

  #[tokio::test]
  async fn a_reply_does_not_discard_a_partially_received_upload_frame() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let (client, server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = open(client, port).await;
    forwarding::write_datagram(&mut writer, &[1]).await.unwrap();
    let mut buffer = [0; 16];
    let (_, address) = echo.recv_from(&mut buffer).await.unwrap();
    writer.write_all(&[0, 0]).await.unwrap();
    writer.flush().await.unwrap();
    echo.send_to(&[2], address).await.unwrap();
    assert_eq!(
      forwarding::read_datagram(&mut reader).await.unwrap(),
      Some(vec![2])
    );
    writer.write_all(&[0, 1, 3]).await.unwrap();
    writer.flush().await.unwrap();
    let (count, _) = tokio::time::timeout(Duration::from_secs(2), echo.recv_from(&mut buffer))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(&buffer[..count], &[3]);
    writer.shutdown().await.unwrap();
    server.await.unwrap();
  }

  #[tokio::test]
  async fn rejects_invalid_destination_before_udp_setup() {
    let (client, server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = tokio::io::split(client);
    let mut preface = vec![0; forwarding::PREFACE.len()];
    reader.read_exact(&mut preface).await.unwrap();
    forwarding::negotiate_contract(&mut reader, &mut writer)
      .await
      .unwrap();
    forwarding::write(
      &mut writer,
      &Request {
        host: "localhost;id".into(),
        port: 53,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      forwarding::read::<Response>(&mut reader).await.unwrap(),
      Response::Error { .. }
    ));
    server.await.unwrap();
  }
}
