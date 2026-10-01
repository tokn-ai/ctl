//! Local listener readiness, independent of tailnet login or remote reachability.

use std::io;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub(super) async fn ready(port: u16) -> io::Result<()> {
  timeout(super::COMMAND_TIMEOUT, check(port)).await?
}

async fn check(port: u16) -> io::Result<()> {
  let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
  stream.write_all(&[5, 1, 0]).await?;
  let mut negotiation = [0; 2];
  stream.read_exact(&mut negotiation).await?;
  if negotiation != [5, 0] {
    return Err(io::Error::other("SOCKS5 listener is not ready"));
  }

  // Closing after negotiation leaves tailscaled waiting for a request header
  // and produces an error on every poll. Complete a UDP ASSOCIATE exchange
  // instead. RFC 1928 permits an unspecified client endpoint (0.0.0.0:0).
  // No UDP datagrams are sent and no destination is contacted. Closing the TCP
  // control connection terminates this temporary association normally.
  stream.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
  let mut header = [0; 4];
  stream.read_exact(&mut header).await?;
  if header[..3] != [5, 0, 0] {
    return Err(io::Error::other(
      "SOCKS5 listener rejected the readiness request",
    ));
  }
  let address_length = match header[3] {
    1 => 4,
    4 => 16,
    3 => match stream.read_u8().await? {
      0 => return Err(invalid_reply()),
      length => usize::from(length),
    },
    _ => return Err(invalid_reply()),
  };
  // Consume the full bound address and port before closing so the server sees
  // a clean EOF, not a reset caused by unread reply bytes.
  let mut address = [0; 257];
  stream
    .read_exact(&mut address[..address_length + 2])
    .await?;
  stream.shutdown().await
}

fn invalid_reply() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "SOCKS5 listener returned an invalid readiness reply",
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::net::Ipv4Addr;
  use std::time::Duration;
  use tokio::net::{TcpListener, TcpStream};

  async fn accept(listener: &TcpListener) -> TcpStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut greeting = [0; 3];
    stream.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, [5, 1, 0]);
    stream
  }

  async fn request(stream: &mut TcpStream) {
    stream.write_all(&[5, 0]).await.unwrap();
    let mut request = [0; 10];
    stream.read_exact(&mut request).await.unwrap();
    assert_eq!(request, [5, 3, 0, 1, 0, 0, 0, 0, 0, 0]);
  }

  #[tokio::test]
  async fn polls_complete_an_association_and_close_without_sending_datagrams() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
      for reply in [
        vec![5, 0, 0, 1, 127, 0, 0, 1, 123, 45],
        vec![
          5, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 123, 45,
        ],
        vec![
          5, 0, 0, 3, 9, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't', 123, 45,
        ],
      ] {
        let mut stream = accept(&listener).await;
        request(&mut stream).await;
        // Fragmented replies must be consumed completely, including the port.
        for byte in reply {
          stream.write_all(&[byte]).await.unwrap();
          tokio::task::yield_now().await;
        }
        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
      }
    });
    for _ in 0..3 {
      ready(port).await.unwrap();
    }
    server.await.unwrap();
  }

  #[tokio::test]
  async fn a_greeting_alone_or_a_rejected_or_malformed_request_is_not_ready() {
    for reply in [
      vec![],                              // EOF after method negotiation, before the request reply.
      vec![5, 7, 0, 1, 0, 0, 0, 0, 0, 0],  // Command rejected.
      vec![4, 0, 0, 1],                    // Wrong version.
      vec![5, 0, 1, 1],                    // Nonzero reserved byte.
      vec![5, 0, 0, 9],                    // Unknown address type.
      vec![5, 0, 0, 3, 0],                 // Empty domain.
      vec![5, 0, 0, 1, 127, 0, 0, 1, 123], // Truncated port.
    ] {
      let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
      let port = listener.local_addr().unwrap().port();
      let server = tokio::spawn(async move {
        let mut stream = accept(&listener).await;
        request(&mut stream).await;
        stream.write_all(&reply).await.unwrap();
      });
      assert!(ready(port).await.is_err());
      server.await.unwrap();
    }
  }

  #[tokio::test]
  async fn rejected_authentication_does_not_send_a_request() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
      let mut stream = accept(&listener).await;
      stream.write_all(&[5, 255]).await.unwrap();
      assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
    });
    assert!(ready(port).await.is_err());
    server.await.unwrap();
  }

  #[tokio::test]
  async fn cancelling_a_stalled_check_closes_its_control_connection() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
      let mut stream = accept(&listener).await;
      request(&mut stream).await;
      stream.write_all(&[5, 0, 0, 1]).await.unwrap();
      assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
    });
    assert!(
      timeout(Duration::from_millis(100), ready(port))
        .await
        .is_err()
    );
    server.await.unwrap();
  }
}
