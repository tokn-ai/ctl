//! Prepared restart of an idle task owner, including older control-v1 owners.

use super::{ClientError, daemon_executable, retryable, spawn_daemon, wait_for_endpoint};
use component_info::{ComponentInfo, ProtocolInfo, executable::PreparedExecutable};
use std::path::PathBuf;
use std::time::Duration;
use task_ipc::{Stream, connect};
use task_proto::{control, read_frame, write_frame};
use tokio::io::AsyncReadExt as _;
use tokio::time::{Instant, timeout};

pub const CONFIRMATION_TTL: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct LifecycleError {
  code: &'static str,
  message: String,
  may_have_stopped: bool,
}

impl LifecycleError {
  #[must_use]
  pub fn code(&self) -> &'static str {
    self.code
  }

  #[must_use]
  pub fn may_have_stopped(&self) -> bool {
    self.may_have_stopped
  }

  fn new(code: &'static str, message: impl std::fmt::Display) -> Self {
    Self {
      code,
      message: message.to_string(),
      may_have_stopped: false,
    }
  }

  fn destructive(mut self) -> Self {
    self.may_have_stopped = true;
    self
  }
}

#[derive(Debug, Clone)]
pub struct Client {
  socket: PathBuf,
  executable: Option<PathBuf>,
}

impl Client {
  #[must_use]
  pub fn new(socket: PathBuf) -> Self {
    Self {
      socket,
      executable: None,
    }
  }

  #[must_use]
  pub fn with_daemon_executable(mut self, executable: PathBuf) -> Self {
    self.executable = Some(executable);
    self
  }

  async fn replacement(&self) -> Result<PreparedExecutable, LifecycleError> {
    let selected = match &self.executable {
      Some(path) => path.clone(),
      None => daemon_executable()
        .map_err(|error| LifecycleError::new("taskd_replacement_unavailable", error))?,
    };
    PreparedExecutable::prepare(
      selected,
      &[
        ("task", task_proto::PROTOCOL_VERSION),
        ("task_control", control::PROTOCOL_VERSION),
      ],
    )
    .await
    .map_err(|error| LifecycleError::new("taskd_replacement_unavailable", error))
  }

  /// Pins an existing owner without sending a mutating request or starting a daemon.
  ///
  /// Older owners expect `RestartDaemon` as their first frame. Retain an unsent
  /// stream so they remain upgradeable without a new diagnostics protocol.
  ///
  /// # Errors
  /// Rejects absent endpoints and unavailable/incompatible replacement helpers.
  pub async fn preflight_restart(&self) -> Result<PreparedRestart, LifecycleError> {
    let replacement = self.replacement().await?;
    let stream = connect(&self.socket)
      .await
      .map_err(|error| LifecycleError::new("taskd_not_running", error))?;
    Ok(PreparedRestart {
      available: replacement.info.clone(),
      replacement,
      stream,
      socket: self.socket.clone(),
      expires_at: Instant::now() + CONFIRMATION_TTL,
    })
  }
}

#[derive(Debug)]
pub struct PreparedRestart {
  pub available: ComponentInfo,
  pub expires_at: Instant,
  replacement: PreparedExecutable,
  socket: PathBuf,
  stream: Stream,
}

#[derive(Debug, Clone)]
pub struct RestartOutcome {
  pub after: ComponentInfo,
}

/// Prepares the currently selected task owner without changing tasks.
///
/// # Errors
/// Rejects absent owners and unavailable replacement helpers.
pub async fn preflight_restart() -> Result<PreparedRestart, LifecycleError> {
  Client::new(task_ipc::socket_path())
    .preflight_restart()
    .await
}

/// Prepares an explicitly selected task owner and replacement helper.
///
/// # Errors
/// Rejects absent owners and unavailable replacement helpers without mutation.
pub async fn preflight_restart_at(
  socket: PathBuf,
  executable: PathBuf,
) -> Result<PreparedRestart, LifecycleError> {
  Client::new(socket)
    .with_daemon_executable(executable)
    .preflight_restart()
    .await
}

impl PreparedRestart {
  /// Requests cooperative idle-only shutdown, preserving the owner's storage and rmux endpoint.
  ///
  /// # Errors
  /// Busy and unsupported owners remain untouched. Reports shutdown, startup and verification errors.
  pub async fn restart(mut self) -> Result<RestartOutcome, LifecycleError> {
    if Instant::now() >= self.expires_at {
      return Err(LifecycleError::new(
        "taskd_restart_expired",
        "Restart confirmation expired; prepare it again",
      ));
    }
    self
      .replacement
      .verify()
      .await
      .map_err(|error| LifecycleError::new("taskd_binary_changed", error))?;
    if timeout(Duration::from_millis(10), self.stream.read(&mut [0_u8; 1]))
      .await
      .is_ok()
    {
      return Err(LifecycleError::new(
        "taskd_owner_changed",
        "The prepared taskd connection closed or changed; prepare the restart again",
      ));
    }
    if Instant::now() >= self.expires_at {
      return Err(LifecycleError::new(
        "taskd_restart_expired",
        "Restart confirmation expired; prepare it again",
      ));
    }
    let configuration = timeout(Duration::from_secs(5), async {
      write_frame(&mut self.stream, &control::ClientMessage::RestartDaemon {
        protocol_version: control::PROTOCOL_VERSION,
      }).await.map_err(|error| LifecycleError::new("taskd_restart_failed", error).destructive())?;
      match read_frame::<_, control::ServerMessage>(&mut self.stream).await {
        Ok(Some(control::ServerMessage::RestartAccepted { data_directory, rmux_socket })) => Ok((data_directory, rmux_socket)),
        Ok(Some(control::ServerMessage::Error { message })) => Err(LifecycleError::new("taskd_restart_rejected", message)),
        _ => Err(LifecycleError::new("taskd_restart_unsupported", "The task owner did not acknowledge cooperative restart; it may require a one-time manual stop").destructive()),
      }
    }).await.map_err(|_| LifecycleError::new("taskd_restart_timeout", "taskd did not acknowledge restart").destructive())??;
    // EOF is sent only after the listener and persisted-state lock are released.
    let end = timeout(Duration::from_secs(15), self.stream.read(&mut [0_u8; 1]))
      .await
      .map_err(|_| {
        LifecycleError::new(
          "taskd_restart_drain_failed",
          "taskd did not finish stopping; no replacement was started",
        )
        .destructive()
      })?
      .map_err(|error| LifecycleError::new("taskd_restart_drain_failed", error).destructive())?;
    if end != 0 {
      return Err(
        LifecycleError::new(
          "taskd_restart_drain_failed",
          "Unexpected data while waiting for taskd shutdown",
        )
        .destructive(),
      );
    }
    self
      .replacement
      .verify()
      .await
      .map_err(|error| LifecycleError::new("taskd_binary_changed", error).destructive())?;
    spawn_daemon(
      &self.socket,
      &self.replacement.path,
      Some((&configuration.0, &configuration.1)),
    )
    .map_err(|error| LifecycleError::new("taskd_restart_start_failed", error).destructive())?;
    let after = verify_successor(&self.socket, &self.replacement)
      .await
      .map_err(LifecycleError::destructive)?;
    Ok(RestartOutcome { after })
  }
}

async fn verify_successor(
  socket: &std::path::Path,
  replacement: &PreparedExecutable,
) -> Result<ComponentInfo, LifecycleError> {
  timeout(Duration::from_secs(8), async {
    let mut stream = wait_for_endpoint(socket)
      .await
      .map_err(|error| LifecycleError::new("taskd_restart_verification_failed", error))?;
    write_frame(
      &mut stream,
      &control::ClientMessage::ComponentStatus {
        protocol_version: control::PROTOCOL_VERSION,
      },
    )
    .await
    .map_err(|error| LifecycleError::new("taskd_restart_verification_failed", error))?;
    let response = read_frame::<_, control::ServerMessage>(&mut stream)
      .await
      .map_err(|error| LifecycleError::new("taskd_restart_verification_failed", error))?;
    if let Some(control::ServerMessage::ComponentStatus {
      build,
      protocol_version,
    }) = response
    {
      let after = ComponentInfo {
        build,
        protocols: vec![
          ProtocolInfo {
            name: "task".into(),
            version: protocol_version,
          },
          ProtocolInfo {
            name: "task_control".into(),
            version: control::PROTOCOL_VERSION,
          },
        ],
      };
      if replacement.matches(&after) {
        return Ok(after);
      }
    }
    Err(LifecycleError::new(
      "taskd_restart_verification_failed",
      "The replacement taskd does not match the verified build and protocols",
    ))
  })
  .await
  .map_err(|_| {
    LifecycleError::new(
      "taskd_restart_verification_failed",
      "The replacement taskd did not answer its version query",
    )
  })?
}

/// Restarts the idle local daemon, or starts one when it is absent.
///
/// # Errors
/// Returns an error for busy/unsupported owners or an unverified replacement.
pub async fn restart_daemon() -> Result<(), ClientError> {
  let client = Client::new(task_ipc::socket_path());
  match connect(&client.socket).await {
    Ok(stream) => {
      drop(stream);
      client
        .preflight_restart()
        .await
        .map_err(|error| ClientError::Restart(error.to_string()))?
        .restart()
        .await
        .map_err(|error| ClientError::Restart(error.to_string()))?;
    }
    Err(error) if retryable(&error) => {
      let replacement = client
        .replacement()
        .await
        .map_err(|error| ClientError::Restart(error.to_string()))?;
      spawn_daemon(&client.socket, &replacement.path, None)?;
      verify_successor(&client.socket, &replacement)
        .await
        .map_err(|error| ClientError::Restart(error.to_string()))?;
    }
    Err(error) => return Err(ClientError::Connect(error)),
  }
  Ok(())
}

#[cfg(all(test, unix))]
mod tests;
