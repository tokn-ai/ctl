//! Prepared cooperative restart of one selected ctmuxd owner.

use ctl_core::{
  component::{ComponentBuildInfo, ComponentInfo, LegacyProtocolInfo, ProtocolInfo},
  executable::PreparedExecutable,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tokio::time::{Instant, timeout};

use crate::{LocalControlClientMessage, LocalControlServerMessage, Stream};

const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
/// Leaves room within the owner's thirty-second armed request deadline.
pub const CONFIRMATION_TTL: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningDaemon {
  pub build: Option<ComponentBuildInfo>,
  pub protocols: Vec<ProtocolInfo>,
  pub protocol_version: Option<ctl_core::protocol::ProtocolVersion>,
  pub control_protocol_version: Option<ctl_core::protocol::ProtocolVersion>,
  pub legacy_protocols: Vec<LegacyProtocolInfo>,
  pub restart_supported: bool,
}

impl RunningDaemon {
  #[must_use]
  pub fn component_info(&self) -> Option<ComponentInfo> {
    if !self.legacy_protocols.is_empty() {
      return None;
    }
    let info = ComponentInfo {
      build: self.build.clone()?,
      protocols: self.protocols.clone(),
    };
    info.is_valid().then_some(info)
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

  /// Observes the existing owner without starting a daemon or preparing a restart.
  ///
  /// # Errors
  /// Returns errors for inaccessible or unrecognized owners.
  pub async fn observe(&self) -> Result<Option<RunningDaemon>, LifecycleError> {
    let control = crate::control_socket_path(&self.socket)
      .map_err(|error| LifecycleError::new("ctmuxd_observation_failed", error))?;
    match query_owner(&control).await? {
      Some((_, info)) => Ok(Some(info)),
      None => match crate::connect_existing_daemon(&self.socket).await {
        Err(error) if error.is_endpoint_unavailable() => Ok(None),
        Err(error) => Err(LifecycleError::new("ctmuxd_observation_failed", error)),
        Ok(_) => Err(LifecycleError::new(
          "daemon_restart_unsupported",
          "The running ctmuxd has no supported control endpoint",
        )),
      },
    }
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
        ("ctmux", ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS),
        (
          "ctmux_control",
          crate::LOCAL_CONTROL_SUPPORTED_PROTOCOL_VERSIONS,
        ),
      ],
    )
    .await
    .map_err(|error| LifecycleError::new("ctmuxd_replacement_unavailable", error))?;
    let control = crate::control_socket_path(&self.socket)
      .map_err(|error| LifecycleError::new("daemon_restart_unsupported", error))?;
    let (stream, before) = query_owner(&control).await?.ok_or_else(|| {
      LifecycleError::new(
        "daemon_restart_unsupported",
        "The selected ctmuxd control owner is not running",
      )
    })?;
    if !before.restart_supported {
      return Err(LifecycleError::new(
        "daemon_restart_unsupported",
        "The selected ctmuxd does not support cooperative restart",
      ));
    }
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
        protocol: crate::local_control_offer(),
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
        protocols,
        restart_supported,
        build,
        data_protocol_version,
        ..
      }) if crate::local_control_offer().accepts(protocol_version)
        && ctl_core::component::protocols_are_valid(&protocols)
        && protocols.iter().any(|protocol| {
          protocol.name == "ctmux_control" && protocol.supports(protocol_version)
        })
        && data_protocol_version.is_none_or(|version| {
          protocols
            .iter()
            .any(|protocol| protocol.name == "ctmux" && protocol.version == version)
        }) =>
      {
        Ok(RunningDaemon {
          build,
          protocols,
          protocol_version: data_protocol_version,
          control_protocol_version: Some(protocol_version),
          legacy_protocols: Vec::new(),
          restart_supported,
        })
      }
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

async fn query_owner(
  control: &std::path::Path,
) -> Result<Option<(Stream, RunningDaemon)>, LifecycleError> {
  let connect_error = |error| LifecycleError::new("ctmuxd_observation_failed", error);
  let mut stream = match crate::connect_existing_daemon(control).await {
    Ok(stream) => stream,
    Err(error) if error.is_endpoint_unavailable() => return Ok(None),
    Err(error) => return Err(connect_error(error)),
  };
  match handshake(&mut stream).await {
    Ok(info) => Ok(Some((stream, info))),
    Err(published_error) => {
      drop(stream);
      // Numeric owners reject the published offer before returning a response.
      // A new read-only handshake establishes their actual historical protocol.
      let mut stream = crate::connect_existing_daemon(control)
        .await
        .map_err(connect_error)?;
      match legacy_handshake(&mut stream).await {
        Ok(info) => Ok(Some((stream, info))),
        Err(_) => Err(published_error),
      }
    }
  }
}

async fn legacy_handshake(stream: &mut Stream) -> Result<RunningDaemon, LifecycleError> {
  #[derive(Deserialize)]
  struct LegacyResponse {
    r#type: String,
    protocol_version: u16,
    restart_supported: bool,
    build: Option<ComponentBuildInfo>,
    data_protocol_version: Option<u16>,
  }
  timeout(QUERY_TIMEOUT, async {
    crate::write_local_control_frame(
      stream,
      &serde_json::json!({
        "type": "handshake", "protocol_version": 1,
      }),
    )
    .await
    .map_err(|error| LifecycleError::new("daemon_restart_unsupported", error))?;
    let response: LegacyResponse = crate::read_local_control_frame(stream)
      .await
      .map_err(|error| LifecycleError::new("daemon_restart_unsupported", error))?
      .ok_or_else(|| LifecycleError::new("daemon_restart_unsupported", "Legacy owner closed"))?;
    if response.r#type != "handshake_accepted"
      || response.protocol_version != 1
      || response.data_protocol_version == Some(0)
      || response
        .build
        .as_ref()
        .is_some_and(|build| !build.is_valid())
    {
      return Err(LifecycleError::new(
        "daemon_restart_unsupported",
        "Invalid legacy owner metadata",
      ));
    }
    let mut legacy_protocols = vec![LegacyProtocolInfo {
      name: "ctmux_control".into(),
      version: 1,
    }];
    if let Some(version) = response.data_protocol_version {
      legacy_protocols.push(LegacyProtocolInfo {
        name: "ctmux".into(),
        version,
      });
    }
    Ok(RunningDaemon {
      build: response.build,
      protocols: Vec::new(),
      protocol_version: None,
      control_protocol_version: None,
      legacy_protocols,
      restart_supported: response.restart_supported,
    })
  })
  .await
  .map_err(|_| LifecycleError::new("daemon_restart_unsupported", "Legacy owner did not respond"))?
}

#[cfg(all(test, unix))]
mod tests;
