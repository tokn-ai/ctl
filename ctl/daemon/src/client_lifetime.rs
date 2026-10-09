//! Cancel connection work when its requesting client disappears, without
//! consuming prompt replies from the protocol reader.

use std::io;
use std::os::fd::AsFd as _;
use std::time::Duration;
use tokio::io::Interest;
use tokio::net::UnixStream;

pub(super) struct Monitor(UnixStream);

impl Monitor {
  pub(super) fn new(stream: &UnixStream) -> io::Result<Self> {
    let socket = std::os::unix::net::UnixStream::from(stream.as_fd().try_clone_to_owned()?);
    // The duplicate shares the original socket's nonblocking state.
    Ok(Self(UnixStream::from_std(socket)?))
  }

  pub(super) async fn closed(&self) -> io::Result<()> {
    loop {
      if self.0.ready(Interest::READABLE).await?.is_read_closed() {
        return Ok(());
      }
      let mut byte = [0_u8; 1];
      match self.0.try_io(Interest::READABLE, || {
        rustix::net::recv(&self.0, &mut byte, rustix::net::RecvFlags::PEEK)
          .map(|(_, length)| length)
          .map_err(io::Error::from)
      }) {
        Ok(0) => return Ok(()),
        Ok(_) => {
          // A prompt reply belongs to the request handler. Yield while it is
          // consumed; peeking must never spin on a readable socket.
          tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(error) => return Err(error),
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{ClientMessage, RequestError, State, handle_connection, handshake};
  use std::sync::Arc;
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

  #[tokio::test]
  async fn prompt_replies_are_not_consumed_by_the_disconnect_monitor() {
    let (mut client, mut server) = UnixStream::pair().unwrap();
    let monitor = Monitor::new(&server).unwrap();
    client.write_all(b"reply").await.unwrap();
    let watcher = tokio::spawn(async move { monitor.closed().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!watcher.is_finished());
    let mut reply = [0_u8; 5];
    server.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"reply");
    drop(client);
    tokio::time::timeout(Duration::from_secs(1), watcher)
      .await
      .unwrap()
      .unwrap()
      .unwrap();
  }

  #[tokio::test]
  async fn disconnected_clients_leave_the_host_queue_without_starting_ssh() {
    let state = Arc::new(State::default());
    let target = ctl_ipc::SshTarget {
      destination: "fixture.invalid".into(),
      ssh_config_alias: None,
      use_ssh_config_master: Some(false),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      gateways: vec![],
    };
    let lifecycle = state.target(&target);
    let _busy = lifecycle.lock.lock().await;
    for quiet in [false, true] {
      for _ in 0..32 {
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(handle_connection(server, Arc::clone(&state)));
        handshake(&mut client).await.unwrap();
        let request = if quiet {
          ClientMessage::EnsureMasterQuiet {
            target: target.clone(),
          }
        } else {
          ClientMessage::EnsureMaster {
            target: target.clone(),
          }
        };
        ctl_ipc::write_frame(&mut client, &request).await.unwrap();
        drop(client);
        assert!(matches!(
          tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap(),
          Err(RequestError::ClientClosed)
        ));
      }
    }
    assert!(state.attempts.lock().unwrap().is_empty());
    assert!(state.configured_connections.lock().unwrap().is_empty());
  }
}
