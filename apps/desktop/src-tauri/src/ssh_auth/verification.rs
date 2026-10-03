use crate::error::{CommandErrorDto, CommandResult};
use ctmux_client::{ClientError, ClientIdentity, handshake};
use tokio::io::{AsyncRead, AsyncWrite};

/// A ctl-agent marker alone does not prove that the remote daemon speaks our protocol.
pub async fn verify(
  mut stream: impl AsyncRead + AsyncWrite + Unpin,
  destination: &str,
) -> CommandResult<()> {
  handshake(
    &mut stream,
    &ClientIdentity {
      name: "ctmux-app".into(),
      version: env!("CARGO_PKG_VERSION").into(),
    },
  )
  .await
  .map(|_| ())
  .map_err(|error| match error {
    ClientError::UnexpectedEof => CommandErrorDto::new(
      "remote_ctmux_handshake_closed",
      format!(
        "SSH verified the account on {destination}, but its terminal connection closed before the daemon replied. Reconnect to check again. Component updates preserve running daemons; an older daemon may need a manual restart after its sessions can be ended."
      ),
    ),
    error => CommandErrorDto::client(error),
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctmux_proto::{ClientMessage, ErrorCode, ServerMessage, read_frame, write_frame};

  #[tokio::test]
  async fn rejects_an_incompatible_daemon_during_protocol_verification() {
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
    let error = verify(client, "destination").await.unwrap_err();
    assert_eq!(error.code, "protocol_version_mismatch");
    task.await.unwrap();
  }

  #[tokio::test]
  async fn a_closed_handshake_identifies_the_destination_without_claiming_a_version_mismatch() {
    let (client, mut server) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
      let message: ClientMessage = read_frame(&mut server).await.unwrap().unwrap();
      assert!(matches!(message, ClientMessage::Handshake { .. }));
    });
    let error = verify(client, "final-destination").await.unwrap_err();
    assert_eq!(error.code, "remote_ctmux_handshake_closed");
    assert!(error.message.contains("final-destination"));
    assert!(
      error
        .message
        .contains("Component updates preserve running daemons")
    );
    task.await.unwrap();
  }

  #[tokio::test]
  async fn legacy_handshake_decoder_eof_keeps_the_same_unconfirmed_diagnosis() {
    #[derive(serde::Deserialize)]
    struct LegacyHandshake {
      protocol_version: u16,
    }

    let (client, mut server) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
      let result = read_frame::<_, LegacyHandshake>(&mut server).await;
      match result {
        Ok(Some(handshake)) => panic!("unexpected legacy version: {}", handshake.protocol_version),
        Err(error) => assert!(
          error
            .to_string()
            .contains("missing field `protocol_version`")
        ),
        Ok(None) => panic!("expected a handshake"),
      }
    });
    let error = verify(client, "final-destination").await.unwrap_err();
    assert_eq!(error.code, "remote_ctmux_handshake_closed");
    task.await.unwrap();
  }
}
