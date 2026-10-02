//! Maintenance can only reuse an existing authenticated SSH master.
use std::{path::Path, process::Stdio, time::Duration};

use ctl_proto::maintenance::{
  self, ClientMessage, CtmuxPreparation, CtmuxRestartCompleted, ServerMessage,
};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::{CoreError, RemotePlatform, SSH_PROGRAM, SshConnectionOptions, SshInteraction};

const PREPARE_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  "exec ctl-agent prepare-ctmux-restart",
);
const INSPECT_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  "exec ctl-agent inspect",
);

pub struct PreparedRemoteCtmuxRestart {
  pub info: CtmuxPreparation,
  child: Child,
  stdin: ChildStdin,
  stdout: ChildStdout,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct MaintenanceError {
  pub code: String,
  pub message: String,
  pub may_have_stopped: bool,
}

/// Opens a fixed preparation command on the selected existing master only.
///
/// # Errors
/// Returns validation, missing-master, unsupported-agent, or preparation errors.
pub async fn prepare_ctmux_restart(
  destination: &str,
  options: &SshConnectionOptions,
  control_path: &Path,
  expected_remote_id: &str,
) -> Result<PreparedRemoteCtmuxRestart, MaintenanceError> {
  let command = command(destination, options, control_path, PREPARE_COMMAND)
    .map_err(|error| failure(error, false))?;
  tokio::time::timeout(Duration::from_secs(8), prepare(command, expected_remote_id))
    .await
    .map_err(|_| {
      failure(
        "Remote restart preparation timed out; nothing was restarted.",
        false,
      )
    })?
}

/// Reads installed agent identity without connecting to or starting a daemon.
///
/// # Errors
/// Returns errors when the existing master or passive inspection is unavailable.
pub async fn inspect_agent(
  destination: &str,
  options: &SshConnectionOptions,
  control_path: &Path,
) -> Result<ctl_proto::RemoteIdentity, CoreError> {
  let output = tokio::time::timeout(
    Duration::from_secs(8),
    crate::run_fixed_command(
      command(destination, options, control_path, INSPECT_COMMAND)?,
      &[],
    ),
  )
  .await
  .map_err(|_| CoreError::InvalidSshCommandOutput)??;
  let identity: ctl_proto::RemoteIdentity =
    serde_json::from_slice(&output).map_err(|_| CoreError::InvalidSshCommandOutput)?;
  if !identity.is_valid() {
    return Err(CoreError::InvalidSshCommandOutput);
  }
  Ok(identity)
}

fn command(
  destination: &str,
  options: &SshConnectionOptions,
  control_path: &Path,
  remote_command: &str,
) -> Result<Command, CoreError> {
  crate::validate_ssh_target(destination, options)?;
  if options.remote_platform != RemotePlatform::Unix {
    return Err(CoreError::InvalidSshOption("remote_platform".into()));
  }
  let mut command = Command::new(SSH_PROGRAM);
  let extra = crate::configure_ssh_interaction(
    &mut command,
    &SshInteraction::Multiplexed {
      control_path: control_path.into(),
    },
  );
  command
    .args(extra)
    // Maintenance only reuses this master; its ProxyCommand=false must never
    // prepare or start a local proxy helper or permit a fresh route.
    .args(crate::ssh_base_arguments_with_proxy(
      destination,
      options,
      None,
      false,
    ))
    .arg(remote_command);
  Ok(command)
}

async fn prepare(
  mut command: Command,
  expected_remote_id: &str,
) -> Result<PreparedRemoteCtmuxRestart, MaintenanceError> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  let mut child = command.spawn().map_err(|error| failure(error, false))?;
  let mut stdin = child
    .stdin
    .take()
    .ok_or_else(|| failure("SSH input is unavailable.", false))?;
  let mut stdout = child
    .stdout
    .take()
    .ok_or_else(|| failure("SSH output is unavailable.", false))?;
  maintenance::write(
    &mut stdin,
    &ClientMessage::PrepareCtmuxRestart {
      protocol_version: maintenance::PROTOCOL_VERSION,
      expected_remote_id: expected_remote_id.into(),
    },
  )
  .await
  .map_err(|error| failure(error, false))?;
  let response = maintenance::read(&mut stdout).await.map_err(|_| failure(
    "The installed ctl-agent does not support prepared restart, or its channel closed. Update remote components, reconnect the host, and try again. Nothing was restarted.", false,
  ))?;
  let info = match response {
    ServerMessage::Prepared { info } if valid_preparation(&info, expected_remote_id) => info,
    ServerMessage::Error { code, message, .. } => {
      return Err(MaintenanceError {
        code,
        message,
        may_have_stopped: false,
      });
    }
    _ => {
      return Err(failure(
        "Remote identity or replacement metadata changed; nothing was restarted.",
        false,
      ));
    }
  };
  Ok(PreparedRemoteCtmuxRestart {
    info,
    child,
    stdin,
    stdout,
  })
}

fn valid_preparation(info: &CtmuxPreparation, expected_remote_id: &str) -> bool {
  info.remote_id == expected_remote_id
    && info.available.build.is_valid()
    && info
      .running
      .build
      .as_ref()
      .is_none_or(ctl_core::component::ComponentBuildInfo::is_valid)
    && info
      .available
      .protocols
      .iter()
      .filter(|protocol| protocol.name == "ctmux")
      .count()
      == 1
    && info
      .available
      .protocols
      .iter()
      .filter(|protocol| protocol.name == "ctmux_control")
      .count()
      == 1
}

impl PreparedRemoteCtmuxRestart {
  /// Confirms the pinned preparation and waits for verified replacement metadata.
  ///
  /// # Errors
  /// Returns a typed failure, including whether the owner may have stopped.
  pub async fn restart(mut self) -> Result<CtmuxRestartCompleted, MaintenanceError> {
    tokio::time::timeout(Duration::from_secs(45), async {
      require_waiting_for_confirmation(&mut self.stdout).await?;
      maintenance::write(&mut self.stdin, &ClientMessage::Confirm {})
        .await
        .map_err(|error| failure(error, true))?;
      let response = maintenance::read(&mut self.stdout)
        .await
        .map_err(|error| failure(error, true))?;
      match response {
        ServerMessage::Completed { result }
          if result.after == self.info.available && result.after.build.is_valid() =>
        {
          self
            .child
            .wait()
            .await
            .map_err(|error| failure(error, true))?;
          Ok(result)
        }
        ServerMessage::Error {
          code,
          message,
          may_have_stopped,
        } => Err(MaintenanceError {
          code,
          message,
          may_have_stopped,
        }),
        _ => Err(failure(
          "Remote restart returned invalid replacement metadata; check the host before retrying.",
          true,
        )),
      }
    })
    .await
    .map_err(|_| {
      failure(
        "Remote restart timed out; the owner may have stopped. Check the host before retrying.",
        true,
      )
    })?
  }
}

async fn require_waiting_for_confirmation(
  reader: &mut (impl AsyncRead + Unpin),
) -> Result<(), MaintenanceError> {
  let mut byte = [0_u8; 1];
  // No response is valid between preparation and confirmation. A closed or
  // already-rejected channel is known not to have received a restart request.
  match tokio::time::timeout(Duration::from_millis(10), reader.read(&mut byte)).await {
    Err(_) => Ok(()),
    Ok(_) => Err(failure(
      "The remote restart confirmation expired or its owner closed. Nothing was restarted; refresh About and try again.",
      false,
    )),
  }
}

fn failure(error: impl std::fmt::Display, may_have_stopped: bool) -> MaintenanceError {
  MaintenanceError {
    code: "remote_maintenance_failed".into(),
    message: error.to_string(),
    may_have_stopped,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn expired_or_rejected_channels_are_known_non_destructive() {
    for mut bytes in [&[][..], &[0_u8][..]] {
      let error = require_waiting_for_confirmation(&mut bytes)
        .await
        .unwrap_err();
      assert!(!error.may_have_stopped);
    }
    let (mut reader, _open_peer) = tokio::io::duplex(16);
    assert!(require_waiting_for_confirmation(&mut reader).await.is_ok());
  }

  #[test]
  fn maintenance_never_falls_back_to_authentication_or_arbitrary_commands() {
    for operation in [PREPARE_COMMAND, INSPECT_COMMAND] {
      let command = command(
        "fixture",
        &SshConnectionOptions::default(),
        Path::new("/unused/owned-master"),
        operation,
      )
      .unwrap();
      let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
      assert!(
        args
          .windows(2)
          .any(|pair| pair == ["-o", "ProxyCommand=false"])
      );
      assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
      assert_eq!(args.last().unwrap(), operation);
      assert!(!operation.contains("connect"));
    }
  }

  #[test]
  fn prepared_identity_and_component_metadata_are_bound() {
    let info: CtmuxPreparation = serde_json::from_value(serde_json::json!({
      "remote_id": "owned-environment",
      "running": {"build": null, "protocol_version": null, "control_protocol_version": 1},
      "available": {
        "build": {"version": "0.1.0", "source_revision": null, "source_fingerprint": "0".repeat(64), "dirty": false},
        "protocols": [{"name": "ctmux", "version": 12}, {"name": "ctmux_control", "version": 1}]
      }
    })).unwrap();
    assert!(valid_preparation(&info, "owned-environment"));
    assert!(!valid_preparation(&info, "different-environment"));
    let mut invalid = info.clone();
    invalid
      .available
      .protocols
      .push(invalid.available.protocols[0].clone());
    assert!(!valid_preparation(&invalid, "owned-environment"));
    let mut invalid = info;
    invalid.available.build.source_fingerprint = "invalid".into();
    assert!(!valid_preparation(&invalid, "owned-environment"));
  }
}
