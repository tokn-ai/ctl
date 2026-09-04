use super::{ClientError, daemon_executable, retryable, spawn_daemon, wait_for_endpoint};
use std::time::Duration;
use task_ipc::connect;
use task_proto::{
  ClientMessage, PROTOCOL_VERSION, ServerMessage, control, read_frame, write_frame,
};
use tokio::io::AsyncReadExt;
use tokio::time::timeout;

/// Restarts the idle local daemon, preserving its storage and rmux endpoint.
/// Starts a daemon if none is running. Never kills an unsupported or busy daemon.
///
/// # Errors
/// Returns an error on active tasks, unsupported control protocol, failed startup,
/// or transport timeout. Older daemons need a one-time manual stop to upgrade.
pub async fn restart_daemon() -> Result<(), ClientError> {
  timeout(Duration::from_secs(20), restart())
    .await
    .map_err(|_| ClientError::Timeout)?
}

async fn restart() -> Result<(), ClientError> {
  let socket = task_ipc::socket_path();
  let executable = daemon_executable()?;
  match connect(&socket).await {
    Ok(mut stream) => {
      write_frame(
        &mut stream,
        &control::ClientMessage::RestartDaemon {
          protocol_version: control::PROTOCOL_VERSION,
        },
      )
      .await?;
      let response = read_frame::<_, control::ServerMessage>(&mut stream)
        .await
        .map_err(|_| unsupported())?
        .ok_or_else(unsupported)?;
      let control::ServerMessage::RestartAccepted {
        data_directory,
        rmux_socket,
      } = response
      else {
        let control::ServerMessage::Error { message } = response else {
          unreachable!()
        };
        return Err(ClientError::Restart(message));
      };
      // The server closes this connection only after releasing its endpoint and
      // state lock. Acknowledgement alone is not permission to start a successor.
      if stream
        .read(&mut [0_u8; 1])
        .await
        .map_err(ClientError::Connect)?
        != 0
      {
        return Err(ClientError::UnexpectedResponse);
      }
      spawn_daemon(&socket, &executable, Some((&data_directory, &rmux_socket)))?;
    }
    Err(error) if retryable(&error) => spawn_daemon(&socket, &executable, None)?,
    Err(error) => return Err(ClientError::Connect(error)),
  }
  let mut stream = wait_for_endpoint(&socket).await?;
  write_frame(
    &mut stream,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: "taskd-restart".into(),
    },
  )
  .await?;
  match read_frame(&mut stream).await? {
    Some(ServerMessage::HandshakeAccepted { protocol_version })
      if protocol_version == PROTOCOL_VERSION =>
    {
      Ok(())
    }
    Some(ServerMessage::Error { code, message }) => Err(ClientError::Server { code, message }),
    _ => Err(ClientError::UnexpectedResponse),
  }
}

fn unsupported() -> ClientError {
  ClientError::Restart("This taskd does not support cooperative restart. Stop its tasks, stop the old daemon manually once, then run ctl taskd restart again.".into())
}
