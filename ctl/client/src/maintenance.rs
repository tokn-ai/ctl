//! Maintenance can only reuse an existing authenticated SSH master.
use std::{path::Path, process::Stdio, time::Duration};

use ctl_proto::maintenance::{
  self, ClientMessage, CtmuxPreparation, CtmuxRestartCompleted, ServerMessage,
};
use tokio::io::{AsyncRead, AsyncReadExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::{CoreError, RemotePlatform, SSH_PROGRAM, SshConnectionOptions, SshInteraction};

const PREPARE_COMMAND: &str = concat!(
  r#"printf 'ctl-maintenance-v1\n'; "#,
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  "exec ctl-agent prepare-ctmux-restart",
);
const INSPECT_COMMAND: &str = concat!(
  r#"printf 'ctl-command-v1\n'; "#,
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  "exec ctl-agent inspect",
);
const COMPONENTS_COMMAND: &str = concat!(
  r#"printf 'ctl-maintenance-v1\n'; "#,
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  "exec ctl-agent inspect-components",
);

/// Inspects installed companions and existing owners on a pinned SSH master.
///
/// # Errors
/// Rejects missing capabilities, changed identity, and invalid component metadata.
pub async fn inspect_components(
  destination: &str,
  options: &SshConnectionOptions,
  control_path: &Path,
  expected_remote_id: &str,
) -> Result<maintenance::RemoteComponents, MaintenanceError> {
  let command = command(destination, options, control_path, COMPONENTS_COMMAND)
    .map_err(|error| failure(error, false))?;
  tokio::time::timeout(
    Duration::from_secs(10),
    inspect(command, expected_remote_id),
  )
  .await
  .map_err(|_| failure("Remote component inspection timed out.", false))?
}

async fn inspect(
  mut command: Command,
  expected_remote_id: &str,
) -> Result<maintenance::RemoteComponents, MaintenanceError> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  let mut child = command.spawn().map_err(|error| failure(error, false))?;
  let mut stdin = child
    .stdin
    .take()
    .ok_or_else(|| failure("Missing inspection input", false))?;
  let mut stdout = BufReader::new(
    child
      .stdout
      .take()
      .ok_or_else(|| failure("Missing inspection output", false))?,
  );
  crate::ssh_startup::Preface::default()
    .read_marker(&mut stdout, &[b"ctl-maintenance-v1\n"], b"ctl-maintenance-")
    .await
    .map_err(|error| failure(error, false))?;
  maintenance::write(
    &mut stdin,
    &ClientMessage::InspectComponents {
      protocol: maintenance::protocol_offer(),
      expected_remote_id: expected_remote_id.into(),
    },
  )
  .await
  .map_err(|error| failure(error, false))?;
  drop(stdin);
  let response = maintenance::read(&mut stdout)
    .await
    .map_err(|error| failure(error, false))?;
  let snapshot = match response {
    ServerMessage::Components {
      protocol_version,
      snapshot,
    } if protocol_version == maintenance::CONTRACT_V1_0_3
      && valid_snapshot(&snapshot, expected_remote_id) =>
    {
      snapshot
    }
    ServerMessage::Error { code, message, .. } => {
      return Err(MaintenanceError {
        code,
        message,
        may_have_stopped: false,
      });
    }
    _ => {
      return Err(failure(
        "Invalid component inspection or changed remote identity.",
        false,
      ));
    }
  };
  if !child
    .wait()
    .await
    .map_err(|error| failure(error, false))?
    .success()
  {
    return Err(failure(
      "Remote component inspection closed unsuccessfully.",
      false,
    ));
  }
  Ok(snapshot)
}

fn valid_snapshot(snapshot: &maintenance::RemoteComponents, remote_id: &str) -> bool {
  use maintenance::{ComponentKind as Kind, ComponentState as State};
  !remote_id.is_empty()
    && snapshot.remote_id == remote_id
    && snapshot.components.len() == 4
    && [Kind::CtlAgent, Kind::Ctld, Kind::Ctmuxd, Kind::CtlTaskd]
      .iter()
      .all(|kind| {
        snapshot
          .components
          .iter()
          .filter(|row| row.component == *kind)
          .count()
          == 1
      })
    && snapshot.components.iter().all(|row| {
      row
        .installed
        .as_ref()
        .is_none_or(ctl_core::component::ComponentInfo::is_valid)
        && row
          .running
          .as_ref()
          .is_none_or(ctl_core::component::ComponentInfo::is_valid)
        && row.error.as_ref().is_none_or(|error| error.len() <= 4096)
        && match row.state {
          State::Legacy => {
            row.legacy_protocols.len() <= 2
              && row.legacy_protocols.iter().all(|p| {
                p.version > 0 && matches!(p.name.as_str(), "ctld" | "ctmux" | "ctmux_control")
              })
          }
          _ => row.legacy_protocols.is_empty(),
        }
    })
}

pub struct PreparedRemoteCtmuxRestart {
  pub info: CtmuxPreparation,
  child: Child,
  stdin: ChildStdin,
  stdout: BufReader<ChildStdout>,
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
    crate::run_marked_fixed_command(
      command(destination, options, control_path, INSPECT_COMMAND)?,
      &[],
      b"ctl-command-v1\n",
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
  let mut stdout = BufReader::new(
    child
      .stdout
      .take()
      .ok_or_else(|| failure("SSH output is unavailable.", false))?,
  );
  crate::ssh_startup::Preface::default()
    .read_marker(&mut stdout, &[b"ctl-maintenance-v1\n"], b"ctl-maintenance-")
    .await
    .map_err(|error| failure(error, false))?;
  maintenance::write(
    &mut stdin,
    &ClientMessage::PrepareCtmuxRestart {
      protocol: maintenance::protocol_offer(),
      expected_remote_id: expected_remote_id.into(),
    },
  )
  .await
  .map_err(|error| failure(error, false))?;
  let response = maintenance::read(&mut stdout).await.map_err(|_| failure(
    "The installed ctl-agent does not support prepared restart, or its channel closed. Update remote components, reconnect the host, and try again. Nothing was restarted.", false,
  ))?;
  let info = match response {
    ServerMessage::Prepared {
      protocol_version,
      info,
    } if maintenance::protocol_offer().accepts(protocol_version)
      && valid_preparation(&info, expected_remote_id)
      && (info.running.legacy_protocols.is_empty()
        || protocol_version == maintenance::CONTRACT_V1_0_3) =>
    {
      info
    }
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
    && info.available.is_valid()
    && ctl_core::component::protocols_are_valid(&info.running.protocols)
    && if info.running.legacy_protocols.is_empty() {
      info
        .running
        .control_protocol_version
        .is_some_and(|version| {
          info
            .running
            .protocols
            .iter()
            .any(|protocol| protocol.name == "ctmux_control" && protocol.supports(version))
        })
    } else {
      info.running.protocols.is_empty()
        && info.running.control_protocol_version.is_none()
        && info.running.protocol_version.is_none()
        && maintenance::valid_legacy_ctmux(&info.running.legacy_protocols)
    }
    && info.running.protocol_version.is_none_or(|version| {
      info
        .running
        .protocols
        .iter()
        .any(|protocol| protocol.name == "ctmux" && protocol.supports(version))
    })
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
    for operation in [PREPARE_COMMAND, INSPECT_COMMAND, COMPONENTS_COMMAND] {
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
    assert!(PREPARE_COMMAND.starts_with("printf 'ctl-maintenance-v1\\n'; "));
    assert!(INSPECT_COMMAND.starts_with("printf 'ctl-command-v1\\n'; "));
  }

  fn preparation() -> CtmuxPreparation {
    serde_json::from_value(serde_json::json!({
      "remote_id": "owned-environment",
      "running": {"build": null, "protocol_version": null, "control_protocol_version": "1.0.1", "protocols": [ctmux_ipc::local_control_protocol_info()]},
      "available": {
        "build": {"version": "0.1.0", "source_revision": null, "source_fingerprint": "0".repeat(64), "dirty": false},
        "protocols": [
          {"name": "ctmux", "build": 13, "version": "1.0.13", "supported_versions": ["1.0.13"]},
          {"name": "ctmux_control", "build": 1, "version": "1.0.1", "supported_versions": ["1.0.1"]}
        ]
      }
    })).unwrap()
  }

  #[cfg(unix)]
  async fn prepared_fixture(startup: &str, before_response: &str) -> Command {
    use std::fmt::Write as _;
    let mut frame = Vec::new();
    maintenance::write(
      &mut frame,
      &ServerMessage::Prepared {
        protocol_version: maintenance::PROTOCOL_VERSION,
        info: preparation(),
      },
    )
    .await
    .unwrap();
    let encoded = frame.iter().fold(String::new(), |mut encoded, byte| {
      write!(encoded, "\\{byte:03o}").unwrap();
      encoded
    });
    let mut command = Command::new("sh");
    command
      .args([
        "-c",
        "printf '%s' \"$CTL_TEST_STARTUP\"; printf 'ctl-maintenance-v1\\n'; printf '%s' \"$CTL_TEST_BEFORE_RESPONSE\"; printf '%b' \"$CTL_TEST_PREPARED\"; cat >/dev/null",
      ])
      .env("CTL_TEST_STARTUP", startup)
      .env("CTL_TEST_BEFORE_RESPONSE", before_response)
      .env("CTL_TEST_PREPARED", encoded);
    command
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn preparation_preserves_buffered_frames_after_shell_startup_output() {
    let mut prepared = tokio::time::timeout(
      Duration::from_secs(5),
      prepare(
        prepared_fixture("Welcome\n\x1b[32mloading profile\x1b[0m", "").await,
        "owned-environment",
      ),
    )
    .await
    .expect("startup framing must preserve the buffered preparation response")
    .unwrap();
    assert_eq!(prepared.info.remote_id, "owned-environment");
    assert_eq!(prepared.info.available, preparation().available);
    require_waiting_for_confirmation(&mut prepared.stdout)
      .await
      .unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn preparation_rejects_noise_after_readiness_before_confirmation() {
    let error = tokio::time::timeout(
      Duration::from_secs(5),
      prepare(
        prepared_fixture("startup banner\n", "late startup output\n").await,
        "owned-environment",
      ),
    )
    .await
    .unwrap()
    .err()
    .expect("bytes after readiness must remain strict maintenance frames");
    assert!(!error.may_have_stopped);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn unsupported_maintenance_markers_fail_before_confirmation() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf 'ctl-maintenance-v2\\n'; cat >/dev/null"]);
    let error = tokio::time::timeout(
      Duration::from_secs(5),
      prepare(command, "owned-environment"),
    )
    .await
    .unwrap()
    .err()
    .expect("unsupported readiness marker must fail without waiting for a frame");
    assert!(!error.may_have_stopped);
    assert!(error.message.contains("ctl-maintenance-v2"));
  }

  #[test]
  fn numeric_control_evidence_is_separate_from_published_contracts() {
    let mut info = preparation();
    info.running.control_protocol_version = None;
    info.running.protocols.clear();
    info.running.legacy_protocols = vec![
      maintenance::LegacyProtocolInfo {
        name: "ctmux_control".into(),
        version: 1,
      },
      maintenance::LegacyProtocolInfo {
        name: "ctmux".into(),
        version: 13,
      },
    ];
    assert!(valid_preparation(&info, "owned-environment"));
    info.running.control_protocol_version = Some(ctmux_ipc::LOCAL_CONTROL_PROTOCOL_VERSION);
    assert!(!valid_preparation(&info, "owned-environment"));
    info.running.control_protocol_version = None;
    info.running.legacy_protocols[0].version = 2;
    assert!(!valid_preparation(&info, "owned-environment"));
  }

  fn snapshot() -> maintenance::RemoteComponents {
    maintenance::RemoteComponents {
      remote_id: "owned-environment".into(),
      components: [
        maintenance::ComponentKind::CtlAgent,
        maintenance::ComponentKind::Ctld,
        maintenance::ComponentKind::Ctmuxd,
        maintenance::ComponentKind::CtlTaskd,
      ]
      .into_iter()
      .map(|component| maintenance::RemoteComponent {
        component,
        installed: None,
        running: None,
        state: maintenance::ComponentState::Unavailable,
        restart_supported: false,
        legacy_protocols: Vec::new(),
        error: None,
      })
      .collect(),
    }
  }

  #[test]
  fn component_inventory_rejects_changed_identity_duplicate_rows_and_invalid_builds() {
    let mut snapshot = snapshot();
    assert!(valid_snapshot(&snapshot, "owned-environment"));
    assert!(!valid_snapshot(&snapshot, "other-environment"));
    snapshot.components[1].component = snapshot.components[0].component;
    assert!(!valid_snapshot(&snapshot, "owned-environment"));
    snapshot.components[1].component = maintenance::ComponentKind::Ctld;
    let mut installed = preparation().available;
    installed.build.source_fingerprint = "invalid".into();
    snapshot.components[2].installed = Some(installed);
    assert!(!valid_snapshot(&snapshot, "owned-environment"));
  }

  #[test]
  fn prepared_identity_and_component_metadata_are_bound() {
    let info = preparation();
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
