use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use ctld_ipc::SshTarget;
use tokio::process::Command;

use super::target_lifecycle::AttemptStatus;
use super::{
  MASTER_CHECK_TIMEOUT, RequestError, SSH_PROGRAM, ServerMessage, TargetLifecycle,
  append_target_arguments,
};

/// An explicit disconnect takes precedence over a pending observation. A later
/// connect requires fresh evidence, even if the old check completed successfully.
pub(super) async fn connection_status<F>(
  lifecycle: &TargetLifecycle,
  observation: F,
) -> Result<ServerMessage, RequestError>
where
  F: Future<Output = Result<bool, RequestError>>,
{
  let mut attempt = lifecycle.attempt();
  if lifecycle.is_paused() {
    return Ok(paused_status());
  }
  let observed = attempt.run(observation).await;
  // A revision can change during the poll that completes the observation.
  // Read generation and pause together even if the result won the select.
  match attempt.status() {
    AttemptStatus::Paused => Ok(paused_status()),
    AttemptStatus::Superseded => Err(RequestError::MasterObservationFailed(
      "the SSH connection changed during the control check; check status again".into(),
    )),
    AttemptStatus::Current => Ok(ServerMessage::ConnectionStatus {
      connected: observed??,
      manually_disconnected: false,
    }),
  }
}

fn paused_status() -> ServerMessage {
  ServerMessage::ConnectionStatus {
    connected: false,
    manually_disconnected: true,
  }
}

/// Checks only the local multiplexing endpoint. OpenSSH's `-O check` does not
/// start a connection, authenticate, or verify the remote transport's health.
/// A missing endpoint proves absence; command failure alone does not.
pub(super) async fn observe(target: &SshTarget, path: &Path) -> Result<bool, RequestError> {
  observe_with_command(path, check_command(target, path), MASTER_CHECK_TIMEOUT).await
}

fn check_command(target: &SshTarget, path: &Path) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  // The selected socket is sufficient for mux control. Reading user config
  // here would execute Match exec during passive status polls.
  command
    .args(["-F", "none"])
    .arg("-S")
    .arg(path)
    .args(["-O", "check"]);
  append_target_arguments(&mut command, target);
  command
}

fn endpoint_exists(path: &Path) -> Result<bool, RequestError> {
  path.try_exists().map_err(|error| {
    RequestError::MasterObservationFailed(format!("could not inspect the control socket: {error}"))
  })
}

async fn observe_with_command(
  path: &Path,
  mut command: Command,
  timeout: Duration,
) -> Result<bool, RequestError> {
  if !endpoint_exists(path)? {
    return Ok(false);
  }
  command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  let status = tokio::time::timeout(timeout, command.status())
    .await
    .map_err(|_| RequestError::MasterObservationFailed("control command timed out".into()))?
    .map_err(|error| {
      RequestError::MasterObservationFailed(format!("could not run the control command: {error}"))
    })?;
  if status.success() {
    return Ok(true);
  }
  // The endpoint may have disappeared while OpenSSH inspected it. Otherwise
  // its exit status cannot distinguish a stale socket from configuration or
  // permission errors, and stderr wording varies across platforms and locales.
  if !endpoint_exists(path)? {
    return Ok(false);
  }
  Err(RequestError::MasterObservationFailed(format!(
    "control command returned {status}"
  )))
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use crate::{ServerMessage, State, handle_connection, handshake};
  use std::sync::Arc;

  fn fixture_path() -> crate::SocketGuard {
    crate::SocketGuard(
      std::env::temp_dir().join(format!("ctld-observation-{}", uuid::Uuid::new_v4())),
    )
  }

  fn fixture_command(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
  }

  #[test]
  fn exact_control_checks_skip_user_configuration() {
    let target = SshTarget {
      destination: "fixture.invalid".into(),
      ssh_config_alias: Some("fixture.invalid".into()),
      use_ssh_config_master: None,
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      gateways: vec![],
    };
    let command = check_command(&target, Path::new("/tmp/fixture-master"));
    let args: Vec<_> = command.as_std().get_args().collect();
    assert_eq!(
      &args[..6],
      ["-F", "none", "-S", "/tmp/fixture-master", "-O", "check"]
    );
    assert!(!args.contains(&std::ffi::OsStr::new("-G")));
  }

  #[tokio::test]
  async fn paused_target_never_polls_an_observation() {
    let lifecycle = TargetLifecycle::default();
    lifecycle.pause();
    let status = connection_status(&lifecycle, async {
      panic!("a paused target must not run a control check")
    })
    .await
    .unwrap();
    assert!(matches!(
      status,
      ServerMessage::ConnectionStatus {
        connected: false,
        manually_disconnected: true,
      }
    ));
  }

  #[tokio::test]
  async fn pause_during_observation_overrides_success_or_error() {
    for succeeds in [false, true] {
      let lifecycle = TargetLifecycle::default();
      let status = connection_status(&lifecycle, async {
        lifecycle.pause();
        if succeeds {
          Ok(true)
        } else {
          Err(RequestError::MasterObservationFailed(
            "fixture timeout".into(),
          ))
        }
      })
      .await
      .unwrap();
      assert!(matches!(
        status,
        ServerMessage::ConnectionStatus {
          connected: false,
          manually_disconnected: true,
        }
      ));
    }
  }

  #[tokio::test]
  async fn disconnect_then_connect_invalidates_a_completed_observation() {
    let lifecycle = TargetLifecycle::default();
    let error = connection_status(&lifecycle, async {
      lifecycle.pause();
      lifecycle.resume(&lifecycle.attempt()).unwrap();
      Ok(true)
    })
    .await
    .unwrap_err();
    assert_eq!(error.code(), "ssh_status_unknown");
    assert!(!lifecycle.is_paused());
  }

  #[tokio::test]
  async fn disconnect_interrupts_a_pending_observation() {
    let lifecycle = Arc::new(TargetLifecycle::default());
    let worker_lifecycle = Arc::clone(&lifecycle);
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
      connection_status(&worker_lifecycle, async {
        started.send(()).unwrap();
        std::future::pending().await
      })
      .await
    });
    started_rx.await.unwrap();
    lifecycle.pause();
    let status = tokio::time::timeout(Duration::from_secs(1), worker)
      .await
      .unwrap()
      .unwrap()
      .unwrap();
    assert!(matches!(
      status,
      ServerMessage::ConnectionStatus {
        connected: false,
        manually_disconnected: true,
      }
    ));
  }

  #[tokio::test]
  async fn missing_endpoint_is_absent_without_running_a_command() {
    let path = fixture_path();
    let command = Command::new(path.0.join("missing-program"));
    assert!(
      !observe_with_command(&path.0, command, Duration::from_secs(1))
        .await
        .unwrap()
    );
  }

  #[tokio::test]
  async fn successful_control_command_observes_an_available_master() {
    let path = fixture_path();
    std::fs::write(&path.0, "fixture endpoint").unwrap();
    assert!(
      observe_with_command(&path.0, fixture_command("exit 0"), Duration::from_secs(1))
        .await
        .unwrap()
    );
  }

  #[tokio::test]
  async fn failed_control_command_does_not_claim_disconnection() {
    let path = fixture_path();
    std::fs::write(&path.0, "fixture endpoint").unwrap();
    let error = observe_with_command(&path.0, fixture_command("exit 255"), Duration::from_secs(1))
      .await
      .unwrap_err();
    assert_eq!(error.code(), "ssh_status_unknown");
  }

  #[tokio::test]
  async fn unavailable_program_does_not_claim_disconnection() {
    let path = fixture_path();
    std::fs::write(&path.0, "fixture endpoint").unwrap();
    let command = Command::new(path.0.join("missing-program"));
    let error = observe_with_command(&path.0, command, Duration::from_secs(1))
      .await
      .unwrap_err();
    assert_eq!(error.code(), "ssh_status_unknown");
    assert!(
      error
        .to_string()
        .contains("could not run the control command")
    );
  }

  #[tokio::test]
  async fn timed_out_control_command_does_not_claim_disconnection() {
    let path = fixture_path();
    std::fs::write(&path.0, "fixture endpoint").unwrap();
    let error = observe_with_command(
      &path.0,
      fixture_command("exec sleep 5"),
      Duration::from_millis(20),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "ssh_status_unknown");
    assert!(error.to_string().contains("control command timed out"));
  }

  #[tokio::test]
  async fn unobservable_endpoint_is_not_reported_as_missing() {
    let path = fixture_path();
    std::os::unix::fs::symlink(&path.0, &path.0).unwrap();
    let error = observe_with_command(&path.0, fixture_command("exit 0"), Duration::from_secs(1))
      .await
      .unwrap_err();
    assert_eq!(error.code(), "ssh_status_unknown");
    assert!(
      error
        .to_string()
        .contains("could not inspect the control socket")
    );
  }

  #[tokio::test]
  async fn disappearing_endpoint_is_known_absent_after_command_failure() {
    let path = fixture_path();
    std::fs::write(&path.0, "fixture endpoint").unwrap();
    let mut command = fixture_command("rm -- \"$1\"; exit 255");
    command.arg("fixture").arg(&path.0);
    assert!(
      !observe_with_command(&path.0, command, Duration::from_secs(1))
        .await
        .unwrap()
    );
  }

  #[tokio::test]
  async fn connection_status_reports_unknown_over_ipc_without_contacting_the_host() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = SshTarget {
      destination: "127.0.0.1".into(),
      port: Some(listener.local_addr().unwrap().port()),
      use_ssh_config_master: Some(true),
      ssh_config_alias: None,
      hostname: None,
      user: None,
      identity_file: None,
      gateways: Vec::new(),
    };
    let path = fixture_path();
    std::fs::write(&path.0, "invalid socket").unwrap();
    let state = Arc::new(State::default());
    state
      .adopt(
        &target,
        &crate::MasterEndpoint {
          control_path: path.0.clone(),
          shared: true,
          startup: crate::SharedMasterStartup::ExternalOnly,
        },
        None,
      )
      .unwrap();
    let (mut client, server) = ctld_ipc::Stream::pair().unwrap();
    let server = tokio::spawn(handle_connection(server, state));
    handshake(&mut client).await.unwrap();
    ctld_ipc::write_frame(
      &mut client,
      &ctld_ipc::ClientMessage::ConnectionStatus { target },
    )
    .await
    .unwrap();
    let response = tokio::select! {
      biased;
      accepted = listener.accept() => {
        drop(accepted);
        panic!("passive master observation attempted a fresh SSH connection");
      }
      response = tokio::time::timeout(
        Duration::from_secs(5),
        ctld_ipc::read_frame::<_, ServerMessage>(&mut client),
      ) => response.unwrap().unwrap(),
    };
    assert!(matches!(
      response,
      Some(ServerMessage::Error { code, .. }) if code == "ssh_status_unknown"
    ));
    assert_eq!(
      server.await.unwrap().unwrap_err().code(),
      "ssh_status_unknown"
    );
  }
}
