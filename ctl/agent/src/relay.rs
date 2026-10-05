//! Drain final daemon output after its input side closes, without holding a
//! disconnected SSH client or a stalled peer open indefinitely.
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn copy<R, W, DR, DW>(
  mut client_reader: R,
  mut client_writer: W,
  mut daemon_reader: DR,
  mut daemon_writer: DW,
) -> io::Result<()>
where
  R: AsyncRead + Unpin,
  W: AsyncWrite + Unpin,
  DR: AsyncRead + Unpin,
  DW: AsyncWrite + Unpin,
{
  let client_to_daemon = tokio::io::copy(&mut client_reader, &mut daemon_writer);
  let daemon_to_client = tokio::io::copy(&mut daemon_reader, &mut client_writer);
  tokio::pin!(client_to_daemon, daemon_to_client);
  tokio::select! {
    result = &mut client_to_daemon => match result {
      Err(error) if matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::NotConnected) => {
        // A late presentation acknowledgement can fail after the daemon has
        // queued final output and SessionEnded. Retain this copy future's
        // buffered bytes and let it reach EOF before closing SSH stdout.
        match tokio::time::timeout(DRAIN_TIMEOUT, &mut daemon_to_client).await {
          Ok(result) => result.map(|_| ()),
          Err(_) => Err(error),
        }
      }
      result => result.map(|_| ()),
    },
    result = &mut daemon_to_client => result.map(|_| ()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
  use tokio::time::Instant;

  #[tokio::test]
  async fn a_late_acknowledgement_does_not_discard_pending_final_output() {
    let (mut output, client_writer) = tokio::io::duplex(1);
    let (mut daemon, daemon_reader) = tokio::io::duplex(1024);
    let final_output = b"final output and session end";
    daemon.write_all(final_output).await.unwrap();
    daemon.shutdown().await.unwrap();
    let (daemon_writer, closed_peer) = tokio::io::duplex(1);
    drop(closed_peer);
    // A one-byte client buffer keeps forwarding pending until the client reads.
    // The failed daemon write therefore wins the relay's initial select.
    let gateway = tokio::spawn(copy(
      &b"late acknowledgement"[..],
      client_writer,
      daemon_reader,
      daemon_writer,
    ));
    let mut received = Vec::new();
    output.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, final_output);
    gateway.await.unwrap().unwrap();
  }

  #[tokio::test(start_paused = true)]
  async fn a_peer_that_rejects_input_but_keeps_output_open_has_a_bounded_drain() {
    let (open_daemon, daemon_reader) = tokio::io::duplex(1);
    let (daemon_writer, closed_peer) = tokio::io::duplex(1);
    drop(closed_peer);
    let started = Instant::now();
    let error = copy(
      &b"late acknowledgement"[..],
      tokio::io::sink(),
      daemon_reader,
      daemon_writer,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(started.elapsed(), DRAIN_TIMEOUT);
    drop(open_daemon);
  }

  #[tokio::test(start_paused = true)]
  async fn an_ssh_disconnect_still_closes_the_gateway_immediately() {
    let (open_daemon, daemon_reader) = tokio::io::duplex(1);
    let started = Instant::now();
    copy(
      tokio::io::empty(),
      tokio::io::sink(),
      daemon_reader,
      tokio::io::sink(),
    )
    .await
    .unwrap();
    assert_eq!(started.elapsed(), Duration::ZERO);
    drop(open_daemon);
  }

  #[tokio::test(start_paused = true)]
  async fn a_failed_ssh_output_does_not_wait_for_the_daemon() {
    let (open_client, client_reader) = tokio::io::duplex(1);
    let (client_writer, closed_client) = tokio::io::duplex(1);
    drop(closed_client);
    let started = Instant::now();
    let error = copy(
      client_reader,
      client_writer,
      &b"reply"[..],
      tokio::io::sink(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(started.elapsed(), Duration::ZERO);
    drop(open_client);
  }
}
