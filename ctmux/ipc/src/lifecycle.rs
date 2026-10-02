//! Prepared cooperative restart of one selected ctmuxd owner.

use ctl_core::{
  component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo},
  executable::PreparedExecutable,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tokio::time::{Instant, timeout};

use crate::{LocalControlClientMessage, LocalControlServerMessage, Stream};

const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
/// Older control-v1 owners wait thirty seconds for the armed request.
pub const CONFIRMATION_TTL: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningDaemon {
  pub build: Option<ComponentBuildInfo>,
  pub protocol_version: Option<u16>,
  pub control_protocol_version: u16,
}

impl RunningDaemon {
  #[must_use]
  pub fn component_info(&self) -> Option<ComponentInfo> {
    Some(ComponentInfo {
      build: self.build.clone()?,
      protocols: vec![
        ProtocolInfo {
          name: "ctmux".into(),
          version: self.protocol_version?,
        },
        ProtocolInfo {
          name: "ctmux_control".into(),
          version: self.control_protocol_version,
        },
      ],
    })
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartOutcome {
  pub after: ComponentInfo,
  pub terminated_sessions: u32,
}

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

  /// Pins an existing cooperative owner and checks its replacement without mutation.
  ///
  /// # Errors
  /// Fails for absent/unsupported owners or an unavailable replacement.
  pub async fn preflight_restart(&self) -> Result<PreparedRestart, LifecycleError> {
    let executable = match &self.executable {
      Some(path) => path.clone(),
      None => crate::daemon_executable()
        .map_err(|error| LifecycleError::new("ctmuxd_replacement_unavailable", error))?,
    };
    let replacement = PreparedExecutable::prepare(
      executable,
      &[
        ("ctmux", ctmux_proto::PROTOCOL_VERSION),
        ("ctmux_control", crate::LOCAL_CONTROL_PROTOCOL_VERSION),
      ],
    )
    .await
    .map_err(|error| LifecycleError::new("ctmuxd_replacement_unavailable", error))?;
    let control = crate::control_socket_path(&self.socket)
      .map_err(|error| LifecycleError::new("daemon_restart_unsupported", error))?;
    let mut stream = crate::connect_existing_daemon(&control)
      .await
      .map_err(|error| {
        LifecycleError::new(
          "daemon_restart_unsupported",
          format!("The selected ctmuxd cannot be restarted cooperatively: {error}"),
        )
      })?;
    let before = handshake(&mut stream).await?;
    Ok(PreparedRestart {
      available: replacement.info.clone(),
      before,
      replacement,
      socket: self.socket.clone(),
      control,
      stream,
      expires_at: Instant::now() + CONFIRMATION_TTL,
    })
  }
}

#[derive(Debug)]
pub struct PreparedRestart {
  pub before: RunningDaemon,
  pub available: ComponentInfo,
  pub expires_at: Instant,
  replacement: PreparedExecutable,
  socket: PathBuf,
  control: PathBuf,
  stream: Stream,
}

impl PreparedRestart {
  /// Replaces the pinned owner, then verifies the selected successor's metadata.
  ///
  /// # Errors
  /// Reports expired/changed preflight, graceful drain or successor verification failure.
  pub async fn restart(mut self) -> Result<RestartOutcome, LifecycleError> {
    self.ensure_unexpired()?;
    self
      .replacement
      .verify()
      .await
      .map_err(|error| LifecycleError::new("ctmuxd_binary_changed", error))?;
    // A peer that already closed cannot be the owner we are about to mutate.
    // A canceled read consumes nothing; healthy armed owners send no data here.
    if timeout(Duration::from_millis(10), self.stream.read(&mut [0_u8; 1]))
      .await
      .is_ok()
    {
      return Err(LifecycleError::new(
        "ctmuxd_owner_changed",
        "The prepared ctmuxd connection closed or changed; prepare the restart again",
      ));
    }
    self.ensure_unexpired()?;
    let response = timeout(QUERY_TIMEOUT, async {
      crate::write_local_control_frame(&mut self.stream, &LocalControlClientMessage::RestartDaemon)
        .await?;
      crate::read_local_control_frame(&mut self.stream).await
    })
    .await
    .map_err(|_| {
      LifecycleError::new(
        "ctmuxd_restart_timeout",
        "ctmuxd did not acknowledge restart",
      )
      .destructive()
    })?
    .map_err(|error| LifecycleError::new("ctmuxd_restart_failed", error).destructive())?;
    let terminated_sessions = match response {
      Some(LocalControlServerMessage::RestartAccepted {
        terminated_sessions,
      }) => terminated_sessions,
      Some(LocalControlServerMessage::Error { code, message }) => {
        let error = LifecycleError::new("ctmuxd_restart_rejected", message);
        return Err(if code == crate::LocalControlErrorCode::Internal {
          error.destructive()
        } else {
          error
        });
      }
      _ => {
        return Err(
          LifecycleError::new(
            "ctmuxd_restart_failed",
            "Unexpected ctmuxd restart response",
          )
          .destructive(),
        );
      }
    };
    drop(self.stream);
    crate::wait_for_daemon_shutdown(&self.socket, &self.control, Duration::from_secs(15))
      .await
      .map_err(|error| LifecycleError::new("ctmuxd_restart_drain_failed", error).destructive())?;
    self
      .replacement
      .verify()
      .await
      .map_err(|error| LifecycleError::new("ctmuxd_binary_changed", error).destructive())?;
    // Bootstrap with the exact executable verified above, never a newly resolved PATH helper.
    let data = crate::connect_or_start_with(
      || crate::connect(&self.socket),
      || crate::start_daemon_with_executable(&self.socket, &self.replacement.path),
      crate::CONNECT_TIMEOUT,
      crate::CONNECT_RETRY_INTERVAL,
    )
    .await
    .map_err(|error| LifecycleError::new("ctmuxd_restart_start_failed", error).destructive())?;
    drop(data);
    let mut successor = crate::connect_existing_daemon(&self.control)
      .await
      .map_err(|error| {
        LifecycleError::new("ctmuxd_restart_verification_failed", error).destructive()
      })?;
    let after = handshake(&mut successor)
      .await
      .map_err(LifecycleError::destructive)?
      .component_info()
      .filter(|info| self.replacement.matches(info))
      .ok_or_else(|| {
        LifecycleError::new(
          "ctmuxd_restart_verification_failed",
          "The replacement ctmuxd does not match the verified build and protocols",
        )
        .destructive()
      })?;
    Ok(RestartOutcome {
      after,
      terminated_sessions,
    })
  }

  fn ensure_unexpired(&self) -> Result<(), LifecycleError> {
    if Instant::now() >= self.expires_at {
      return Err(LifecycleError::new(
        "ctmuxd_restart_expired",
        "Restart confirmation expired; prepare it again",
      ));
    }
    Ok(())
  }
}

async fn handshake(stream: &mut Stream) -> Result<RunningDaemon, LifecycleError> {
  timeout(QUERY_TIMEOUT, async {
    crate::write_local_control_frame(
      stream,
      &LocalControlClientMessage::Handshake {
        protocol_version: crate::LOCAL_CONTROL_PROTOCOL_VERSION,
      },
    )
    .await
    .map_err(|error| LifecycleError::new("daemon_restart_unsupported", error))?;
    match crate::read_local_control_frame(stream)
      .await
      .map_err(|error| LifecycleError::new("daemon_restart_unsupported", error))?
    {
      Some(LocalControlServerMessage::HandshakeAccepted {
        protocol_version,
        restart_supported: true,
        build,
        data_protocol_version,
        ..
      }) if protocol_version == crate::LOCAL_CONTROL_PROTOCOL_VERSION => Ok(RunningDaemon {
        build,
        protocol_version: data_protocol_version,
        control_protocol_version: protocol_version,
      }),
      _ => Err(LifecycleError::new(
        "daemon_restart_unsupported",
        "The running ctmuxd does not support cooperative restart; stop it manually once to upgrade",
      )),
    }
  })
  .await
  .map_err(|_| {
    LifecycleError::new(
      "daemon_restart_unsupported",
      "ctmuxd did not answer the restart capability query",
    )
  })?
}

#[cfg(all(test, unix))]
mod tests;
