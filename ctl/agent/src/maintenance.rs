//! Two-phase remote ctmux maintenance. Dropping the channel never confirms restart.
use std::{io, time::Duration};

use ctl_proto::maintenance::{
  self, ClientMessage, CtmuxPreparation, CtmuxRestartCompleted, RunningCtmux, ServerMessage,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::ConnectConfig;

/// Inspects the existing owner and installed companion, then waits for confirmation.
///
/// # Errors
/// Returns transport errors. Operation errors are sent as structured responses.
pub async fn prepare_ctmux_restart<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
  reader: &mut R,
  writer: &mut W,
  config: &ConnectConfig,
  actual_remote_id: &str,
) -> io::Result<()> {
  let prepared = prepare(reader, config, actual_remote_id).await;
  let prepared = match prepared {
    Ok(prepared) => prepared,
    Err(error) => {
      return maintenance::write(
        writer,
        &ServerMessage::Error {
          code: "remote_restart_unavailable".into(),
          message: error.to_string(),
          may_have_stopped: false,
        },
      )
      .await;
    }
  };
  maintenance::write(
    writer,
    &ServerMessage::Prepared {
      info: CtmuxPreparation {
        remote_id: actual_remote_id.into(),
        running: RunningCtmux {
          build: prepared.before.build.clone(),
          protocol_version: prepared.before.protocol_version,
          control_protocol_version: prepared.before.control_protocol_version,
        },
        available: prepared.available.clone(),
      },
    },
  )
  .await?;
  // Existing local control clients have a finite idle timeout. Expire first.
  if let Err(error) = await_confirmation(reader).await {
    return maintenance::write(
      writer,
      &ServerMessage::Error {
        code: "remote_confirmation_expired".into(),
        message: error.to_string(),
        may_have_stopped: false,
      },
    )
    .await;
  }
  let response = match prepared.restart().await {
    Ok(outcome) => ServerMessage::Completed {
      result: CtmuxRestartCompleted {
        after: outcome.after,
        terminated_sessions: outcome.terminated_sessions,
      },
    },
    Err(error) => ServerMessage::Error {
      code: error.code().into(),
      message: error.to_string(),
      may_have_stopped: error.may_have_stopped(),
    },
  };
  maintenance::write(writer, &response).await
}

async fn prepare<R: AsyncRead + Unpin>(
  reader: &mut R,
  config: &ConnectConfig,
  actual_remote_id: &str,
) -> io::Result<ctmux_ipc::lifecycle::PreparedRestart> {
  let request = tokio::time::timeout(Duration::from_secs(5), maintenance::read(reader))
    .await
    .map_err(|_| io::Error::other("Maintenance request timed out."))??;
  match request {
    ClientMessage::PrepareCtmuxRestart {
      protocol_version,
      expected_remote_id,
    } if protocol_version == maintenance::PROTOCOL_VERSION
      && !expected_remote_id.is_empty()
      && expected_remote_id == actual_remote_id => {}
    _ => {
      return Err(io::Error::other(
        "Remote identity or maintenance protocol changed; no restart was attempted.",
      ));
    }
  }
  if config.service != crate::Service::Ctmux {
    return Err(io::Error::other(
      "Only the account's ctmux owner can be restarted.",
    ));
  }
  let executable = config
    .ctmuxd_bin
    .clone()
    .filter(|path| path.is_absolute())
    .ok_or_else(|| io::Error::other("Install ctmuxd beside ctl-agent before restarting."))?;
  ctmux_ipc::lifecycle::Client::new(config.ctmux_socket.clone())
    .with_daemon_executable(executable)
    .preflight_restart()
    .await
    .map_err(io::Error::other)
}

async fn await_confirmation<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<()> {
  match tokio::time::timeout(Duration::from_secs(20), maintenance::read(reader)).await {
    Ok(Ok(ClientMessage::Confirm {})) => Ok(()),
    _ => Err(io::Error::other(
      "Restart confirmation expired or closed; no restart was attempted.",
    )),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn closed_channel_or_repeated_preparation_never_confirms() {
    assert!(await_confirmation(&mut [].as_slice()).await.is_err());
    let mut bytes = Vec::new();
    maintenance::write(
      &mut bytes,
      &ClientMessage::PrepareCtmuxRestart {
        protocol_version: maintenance::PROTOCOL_VERSION,
        expected_remote_id: "identity".into(),
      },
    )
    .await
    .unwrap();
    assert!(await_confirmation(&mut bytes.as_slice()).await.is_err());
  }

  #[tokio::test]
  async fn identity_mismatch_is_rejected_before_accessing_any_daemon() {
    let mut bytes = Vec::new();
    maintenance::write(
      &mut bytes,
      &ClientMessage::PrepareCtmuxRestart {
        protocol_version: maintenance::PROTOCOL_VERSION,
        expected_remote_id: "wrong".into(),
      },
    )
    .await
    .unwrap();
    let config = ConnectConfig::new("/unused/maintenance-test.sock".into());
    let mut response = Vec::new();
    prepare_ctmux_restart(&mut bytes.as_slice(), &mut response, &config, "actual")
      .await
      .unwrap();
    assert!(matches!(
      maintenance::read::<_, ServerMessage>(&mut response.as_slice())
        .await
        .unwrap(),
      ServerMessage::Error {
        may_have_stopped: false,
        ..
      }
    ));
  }
}
