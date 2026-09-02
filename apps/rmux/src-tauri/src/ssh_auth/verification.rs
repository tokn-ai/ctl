use crate::error::{CommandErrorDto, CommandResult};
use rmux_client::{ClientIdentity, handshake};
use tokio::io::{AsyncRead, AsyncWrite};

/// A ctld marker alone does not prove that the remote daemon speaks our protocol.
pub async fn verify(mut stream: impl AsyncRead + AsyncWrite + Unpin) -> CommandResult<()> {
  handshake(
    &mut stream,
    &ClientIdentity {
      name: "rmux-app".into(),
      version: env!("CARGO_PKG_VERSION").into(),
    },
  )
  .await
  .map(|_| ())
  .map_err(CommandErrorDto::client)
}

#[cfg(test)]
mod tests {
  use super::*;
  use rmux_proto::{ClientMessage, ErrorCode, ServerMessage, read_frame, write_frame};

  #[tokio::test]
  async fn rejects_an_incompatible_daemon_before_saving_credentials() {
    let (client, mut server) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
      let message: ClientMessage = read_frame(&mut server).await.unwrap().unwrap();
      assert!(matches!(message, ClientMessage::Handshake { .. }));
      write_frame(
        &mut server,
        &ServerMessage::Error {
          code: ErrorCode::ProtocolVersionMismatch,
          message: "incompatible daemon".into(),
        },
      )
      .await
      .unwrap();
    });
    let error = verify(client).await.unwrap_err();
    assert_eq!(error.code, "protocol_version_mismatch");
    task.await.unwrap();
  }
}
