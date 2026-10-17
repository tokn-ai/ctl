//! Two-phase remote ctmux maintenance. Dropping the channel never confirms a destructive operation.
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
  let (protocol_version, prepared) = match prepare(reader, config, actual_remote_id).await {
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
      protocol_version,
      info: CtmuxPreparation {
        remote_id: actual_remote_id.into(),
        running: RunningCtmux {
          build: prepared.before.build.clone(),
          protocol_version: prepared.before.protocol_version,
          control_protocol_version: prepared.before.control_protocol_version,
          protocols: prepared.before.protocols.clone(),
          legacy_protocols: prepared.before.legacy_protocols.clone(),
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

/// Prepares an identity-bound remote shutdown without inspecting a replacement.
///
/// # Errors
/// Returns framing failures; preparation and shutdown failures are structured replies.
pub async fn prepare_ctmux_stop<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
  reader: &mut R,
  writer: &mut W,
  config: &ConnectConfig,
  actual_remote_id: &str,
) -> io::Result<()> {
  let preparation = async {
    let request = tokio::time::timeout(Duration::from_secs(5), maintenance::read(reader))
      .await
      .map_err(|_| io::Error::other("Maintenance request timed out"))??;
    let protocol_version = match request {
      ClientMessage::PrepareCtmuxStop {
        protocol,
        expected_remote_id,
      } if !expected_remote_id.is_empty()
        && expected_remote_id == actual_remote_id
        && config.service == crate::Service::Ctmux =>
      {
        protocol
          .negotiate(&[maintenance::CONTRACT_V1_2_4])
          .ok_or_else(|| io::Error::other("Remote stop requires maintenance 1.2.4"))?
      }
      _ => {
        return Err(io::Error::other(
          "Remote identity or stop contract changed; nothing was stopped",
        ));
      }
    };
    let prepared = ctmux_ipc::lifecycle::Client::new(config.ctmux_socket.clone())
      .preflight_stop()
      .await
      .map_err(io::Error::other)?;
    Ok::<_, io::Error>((protocol_version, prepared))
  }
  .await;
  let (protocol_version, prepared) = match preparation {
    Ok(prepared) => prepared,
    Err(error) => {
      return maintenance::write(
        writer,
        &ServerMessage::Error {
          code: "remote_stop_unavailable".into(),
          message: error.to_string(),
          may_have_stopped: false,
        },
      )
      .await;
    }
  };
  maintenance::write(
    writer,
    &ServerMessage::StopPrepared {
      protocol_version,
      info: maintenance::CtmuxStopPreparation {
        remote_id: actual_remote_id.into(),
        running: RunningCtmux {
          build: prepared.before.build.clone(),
          protocol_version: prepared.before.protocol_version,
          control_protocol_version: prepared.before.control_protocol_version,
          protocols: prepared.before.protocols.clone(),
          legacy_protocols: prepared.before.legacy_protocols.clone(),
        },
      },
    },
  )
  .await?;
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
  let response = match prepared.stop().await {
    Ok(terminated_sessions) => ServerMessage::Stopped {
      result: maintenance::CtmuxStopCompleted {
        terminated_sessions,
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
) -> io::Result<(
  ctl_core::protocol::ProtocolVersion,
  ctmux_ipc::lifecycle::PreparedRestart,
)> {
  let request = tokio::time::timeout(Duration::from_secs(5), maintenance::read(reader))
    .await
    .map_err(|_| io::Error::other("Maintenance request timed out."))??;
  let invalid_request = || {
    io::Error::other("Remote identity or maintenance protocol changed; no restart was attempted.")
  };
  let protocol_version = match request {
    ClientMessage::PrepareCtmuxRestart {
      protocol,
      expected_remote_id,
    } if !expected_remote_id.is_empty() && expected_remote_id == actual_remote_id => protocol
      .negotiate(maintenance::SUPPORTED_PROTOCOL_VERSIONS)
      .ok_or_else(invalid_request)?,
    _ => return Err(invalid_request()),
  };
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
  let prepared = ctmux_ipc::lifecycle::Client::new(config.ctmux_socket.clone())
    .with_daemon_executable(executable)
    .preflight_restart()
    .await
    .map_err(io::Error::other)?;
  if !prepared.before.legacy_protocols.is_empty()
    && ![maintenance::CONTRACT_V1_1_3, maintenance::CONTRACT_V1_2_4].contains(&protocol_version)
  {
    return Err(io::Error::other(
      "Legacy daemon maintenance requires contract 1.1.3; update the client first.",
    ));
  }
  Ok((protocol_version, prepared))
}

async fn await_confirmation<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<()> {
  match tokio::time::timeout(Duration::from_secs(20), maintenance::read(reader)).await {
    Ok(Ok(ClientMessage::Confirm {})) => Ok(()),
    _ => Err(io::Error::other(
      "Maintenance confirmation expired or closed; no shutdown was attempted.",
    )),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn stop_rejects_old_contracts_and_changed_identity_before_daemon_access() {
    for (version, identity) in [
      (maintenance::CONTRACT_V1_1_3, "actual"),
      (maintenance::CONTRACT_V1_2_4, "wrong"),
    ] {
      let mut bytes = Vec::new();
      maintenance::write(
        &mut bytes,
        &ClientMessage::PrepareCtmuxStop {
          protocol: ctl_core::protocol::ProtocolOffer::new(version.build, version, &[version]),
          expected_remote_id: identity.into(),
        },
      )
      .await
      .unwrap();
      let config = ConnectConfig::new("/unused/maintenance-stop.sock".into());
      let mut response = Vec::new();
      prepare_ctmux_stop(&mut bytes.as_slice(), &mut response, &config, "actual")
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

  #[cfg(unix)]
  async fn stop_owner_fixture(
    listener: tokio::net::UnixListener,
    control: std::path::PathBuf,
    confirm: bool,
  ) {
    use ctmux_ipc::{LocalControlClientMessage, LocalControlServerMessage};
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
      ctmux_ipc::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
        .await
        .unwrap(),
      Some(LocalControlClientMessage::Handshake { .. })
    ));
    ctmux_ipc::write_local_control_frame(
      &mut stream,
      &LocalControlServerMessage::HandshakeAccepted {
        protocol_version: ctmux_ipc::LOCAL_CONTROL_PROTOCOL_VERSION,
        protocols: vec![ctmux_ipc::local_control_protocol_info()],
        restart_supported: true,
        managed_sessions_supported: false,
        build: None,
        data_protocol_version: None,
      },
    )
    .await
    .unwrap();
    let request = ctmux_ipc::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
      .await
      .unwrap();
    if confirm {
      assert!(matches!(
        request,
        Some(LocalControlClientMessage::RestartDaemon)
      ));
      ctmux_ipc::write_local_control_frame(
        &mut stream,
        &LocalControlServerMessage::RestartAccepted {
          terminated_sessions: 3,
        },
      )
      .await
      .unwrap();
    } else {
      assert!(request.is_none());
    }
    drop(listener);
    std::fs::remove_file(control).unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn remote_stop_requires_separate_confirmation_and_no_replacement() {
    let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let root = std::env::temp_dir().join(format!(
      "rs-{}",
      &uuid::Uuid::new_v4().simple().to_string()[..12]
    ));
    std::fs::create_dir_all(&root).unwrap();
    for confirm in [false, true] {
      let config = ConnectConfig::new(root.join("ctmux.sock"));
      let control = ctmux_ipc::control_socket_path(&config.ctmux_socket).unwrap();
      let listener = tokio::net::UnixListener::bind(&control).unwrap();
      let server = tokio::spawn(stop_owner_fixture(listener, control, confirm));
      let mut bytes = Vec::new();
      maintenance::write(
        &mut bytes,
        &ClientMessage::PrepareCtmuxStop {
          protocol: maintenance::protocol_offer(),
          expected_remote_id: "actual".into(),
        },
      )
      .await
      .unwrap();
      if confirm {
        maintenance::write(&mut bytes, &ClientMessage::Confirm {})
          .await
          .unwrap();
      }
      let mut response = Vec::new();
      prepare_ctmux_stop(&mut bytes.as_slice(), &mut response, &config, "actual")
        .await
        .unwrap();
      let mut response = response.as_slice();
      assert!(matches!(
        maintenance::read::<_, ServerMessage>(&mut response)
          .await
          .unwrap(),
        ServerMessage::StopPrepared {
          protocol_version: maintenance::CONTRACT_V1_2_4,
          ..
        }
      ));
      let result = maintenance::read::<_, ServerMessage>(&mut response)
        .await
        .unwrap();
      if confirm {
        assert!(
          matches!(result, ServerMessage::Stopped { result } if result.terminated_sessions == 3)
        );
      } else {
        assert!(matches!(
          result,
          ServerMessage::Error {
            may_have_stopped: false,
            ..
          }
        ));
      }
      tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
  }

  #[tokio::test]
  async fn closed_channel_or_repeated_preparation_never_confirms() {
    assert!(await_confirmation(&mut [].as_slice()).await.is_err());
    let mut bytes = Vec::new();
    maintenance::write(
      &mut bytes,
      &ClientMessage::PrepareCtmuxRestart {
        protocol: maintenance::protocol_offer(),
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
        protocol: maintenance::protocol_offer(),
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
