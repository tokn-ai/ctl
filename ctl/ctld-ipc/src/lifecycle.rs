//! Read-only identity queries and cooperative replacement of a selected owner.
//! Lifecycle frames are independent of the SSH/VPN protocol so a newer client
//! can inspect an older data protocol without submitting an SSH or VPN request.

use component_info::{ComponentBuildInfo, ComponentInfo};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};

pub const PROTOCOL_VERSION: u16 = 1;
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonBinaryInfo {
  pub build: ComponentBuildInfo,
  pub protocol_version: u16,
  pub lifecycle_protocol_version: u16,
}

impl DaemonBinaryInfo {
  #[must_use]
  pub fn current() -> Self {
    Self {
      build: component_info::build_info(),
      protocol_version: crate::PROTOCOL_VERSION,
      lifecycle_protocol_version: PROTOCOL_VERSION,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
  pub instance_id: String,
  pub binary: DaemonBinaryInfo,
  pub active_vpn_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DaemonStatus {
  Absent,
  Legacy { protocol_version: Option<u16> },
  Running { info: DaemonInfo },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableDaemon {
  pub executable: PathBuf,
  pub info: DaemonBinaryInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartOutcome {
  pub before: Option<DaemonInfo>,
  pub after: DaemonInfo,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
  CtldInspect { protocol_version: u16 },
  CtldRestart { expected_instance_id: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
  CtldInfo { info: DaemonInfo },
  CtldRestartAccepted { instance_id: String },
  CtldError { code: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
  #[error(transparent)]
  Connect(#[from] crate::ConnectError),
  #[error("ctld lifecycle I/O failed: {0}")]
  Io(#[from] io::Error),
  #[error(transparent)]
  Codec(#[from] crate::CodecError),
  #[error("ctld did not answer the version query in time")]
  Timeout,
  #[error(
    "The running ctld cannot restart cooperatively. Stop the old ctld manually once, then try again."
  )]
  Unsupported,
  #[error("The replacement ctld does not report compatible component metadata: {0}")]
  Unavailable(String),
  #[error("The selected ctld executable changed. Check versions again before restarting.")]
  BinaryChanged,
  #[error("The ctld owner changed. Check versions again before restarting.")]
  OwnerChanged,
  #[error("ctld did not finish stopping in time. No replacement was started.")]
  ShutdownTimeout,
  #[error("The replacement ctld does not match the verified build or protocol.")]
  VerificationFailed,
  #[error("ctld rejected restart: {message}")]
  Rejected { code: String, message: String },
  #[error("ctld returned an unexpected lifecycle response")]
  UnexpectedResponse,
}

impl LifecycleError {
  #[must_use]
  pub fn code(&self) -> &'static str {
    match self {
      Self::Unsupported => "ctld_restart_unsupported",
      Self::BinaryChanged => "ctld_binary_changed",
      Self::OwnerChanged => "ctld_owner_changed",
      Self::Unavailable(_) => "ctld_replacement_unavailable",
      Self::ShutdownTimeout => "ctld_restart_drain_failed",
      Self::VerificationFailed => "ctld_restart_verification_failed",
      Self::Timeout => "ctld_query_timeout",
      _ => "ctld_lifecycle_failed",
    }
  }
}

#[derive(Debug, Clone)]
pub struct Client {
  socket_path: PathBuf,
  executable: Option<PathBuf>,
}

impl Client {
  #[must_use]
  pub fn new(socket_path: PathBuf) -> Self {
    Self {
      socket_path,
      executable: None,
    }
  }

  #[must_use]
  pub fn with_daemon_executable(mut self, executable: PathBuf) -> Self {
    self.executable = Some(executable);
    self
  }

  /// Observes only the selected endpoint. Never starts a daemon.
  ///
  /// # Errors
  /// Returns transport failures and bounded query timeouts.
  pub async fn probe(&self) -> Result<DaemonStatus, LifecycleError> {
    let Some(mut stream) = self.connect_existing().await? else {
      return Ok(DaemonStatus::Absent);
    };
    match inspect(&mut stream).await {
      Ok(info) => Ok(DaemonStatus::Running { info }),
      Err(LifecycleError::Unsupported | LifecycleError::Codec(_)) => Ok(DaemonStatus::Legacy {
        protocol_version: self.legacy_protocol().await,
      }),
      Err(error) => Err(error),
    }
  }

  /// Queries the selected executable without starting its daemon service.
  ///
  /// # Errors
  /// Returns missing, invalid, or unresponsive executable errors.
  pub async fn available(&self) -> Result<AvailableDaemon, LifecycleError> {
    let selected = match &self.executable {
      Some(path) => path.clone(),
      None => crate::daemon_executable()?,
    };
    let executable = resolve_executable(&selected)?;
    let output = timeout(
      QUERY_TIMEOUT,
      tokio::process::Command::new(&executable)
        .arg("--component-info")
        .env_remove("CTLD_ASKPASS")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output(),
    )
    .await
    .map_err(|_| LifecycleError::Timeout)??;
    if !output.status.success() || output.stdout.len() > 16 * 1024 {
      return Err(LifecycleError::Unavailable(
        "update or rebuild ctld together with the app".into(),
      ));
    }
    let metadata: ComponentInfo = serde_json::from_slice(&output.stdout)
      .map_err(|_| LifecycleError::Unavailable("invalid --component-info output".into()))?;
    let protocol = |name: &str| {
      let mut entries = metadata.protocols.iter().filter(|entry| entry.name == name);
      let value = entries.next()?.version;
      entries.next().is_none().then_some(value)
    };
    let info = DaemonBinaryInfo {
      protocol_version: protocol("ctld")
        .ok_or_else(|| LifecycleError::Unavailable("missing ctld protocol".into()))?,
      lifecycle_protocol_version: protocol("ctld_lifecycle")
        .ok_or_else(|| LifecycleError::Unavailable("missing lifecycle protocol".into()))?,
      build: metadata.build,
    };
    if !info.build.is_valid() {
      return Err(LifecycleError::Unavailable("missing build identity".into()));
    }
    Ok(AvailableDaemon { executable, info })
  }

  /// Pins the owner and verifies the replacement before a confirmation is shown.
  ///
  /// # Errors
  /// Rejects legacy owners and unavailable replacement binaries without mutation.
  pub async fn preflight_restart(&self) -> Result<PreparedRestart, LifecycleError> {
    let available = self.available().await?;
    if available.info.protocol_version != crate::PROTOCOL_VERSION
      || available.info.lifecycle_protocol_version != PROTOCOL_VERSION
    {
      return Err(LifecycleError::Unavailable(
        "the selected executable uses an incompatible protocol".into(),
      ));
    }
    let mut stream = self.connect_existing().await?;
    let before = match &mut stream {
      Some(stream) => Some(inspect(stream).await?),
      None => None,
    };
    Ok(PreparedRestart {
      before,
      available,
      client: self.clone(),
      stream,
    })
  }

  async fn connect_existing(&self) -> Result<Option<crate::Stream>, LifecycleError> {
    match crate::connect_existing_at(&self.socket_path).await {
      Ok(stream) => Ok(Some(stream)),
      Err(crate::ConnectError::Connect(error)) if crate::retryable_connect_error(&error) => {
        Ok(None)
      }
      Err(error) => Err(error.into()),
    }
  }

  async fn legacy_protocol(&self) -> Option<u16> {
    let mut stream = self.connect_existing().await.ok()??;
    timeout(QUERY_TIMEOUT, async {
      crate::write_frame(
        &mut stream,
        &crate::ClientMessage::Handshake {
          protocol_version: crate::PROTOCOL_VERSION,
        },
      )
      .await
      .ok()?;
      match crate::read_frame::<_, crate::ServerMessage>(&mut stream)
        .await
        .ok()?
      {
        Some(crate::ServerMessage::HandshakeAccepted { protocol_version }) => {
          Some(protocol_version)
        }
        _ => None,
      }
    })
    .await
    .ok()
    .flatten()
  }
}

pub struct PreparedRestart {
  pub before: Option<DaemonInfo>,
  pub available: AvailableDaemon,
  client: Client,
  stream: Option<crate::Stream>,
}

impl PreparedRestart {
  /// Replaces only the pinned owner and verifies its successor.
  ///
  /// # Errors
  /// Rejects changed owners/binaries and reports graceful-shutdown/start failures.
  pub async fn restart(mut self) -> Result<RestartOutcome, LifecycleError> {
    if self.client.available().await? != self.available {
      return Err(LifecycleError::BinaryChanged);
    }
    match (&self.before, self.stream.as_mut()) {
      (Some(before), Some(stream)) => {
        // A pinned connection belongs to one process even if its endpoint has
        // meanwhile been replaced. Also verify the selected endpoint still
        // points to that same instance before submitting the stop request.
        if !matches!(self.client.probe().await?, DaemonStatus::Running { info } if info.instance_id == before.instance_id)
        {
          return Err(LifecycleError::OwnerChanged);
        }
        timeout(QUERY_TIMEOUT, async {
          crate::write_frame(
            stream,
            &Request::CtldRestart {
              expected_instance_id: before.instance_id.clone(),
            },
          )
          .await?;
          match crate::read_frame::<_, Response>(stream).await? {
            Some(Response::CtldRestartAccepted { instance_id })
              if instance_id == before.instance_id =>
            {
              Ok(())
            }
            Some(Response::CtldError { code, message }) => {
              Err(LifecycleError::Rejected { code, message })
            }
            _ => Err(LifecycleError::UnexpectedResponse),
          }
        })
        .await
        .map_err(|_| LifecycleError::Timeout)??;
        let completion = timeout(SHUTDOWN_TIMEOUT, crate::read_frame::<_, Response>(stream))
          .await
          .map_err(|_| LifecycleError::ShutdownTimeout)??;
        match completion {
          None => {}
          Some(Response::CtldError { code, message }) => {
            return Err(LifecycleError::Rejected { code, message });
          }
          Some(_) => return Err(LifecycleError::UnexpectedResponse),
        }
      }
      (None, None) => {
        if !matches!(self.client.probe().await?, DaemonStatus::Absent) {
          return Err(LifecycleError::OwnerChanged);
        }
      }
      _ => return Err(LifecycleError::UnexpectedResponse),
    }
    // Never unlink an endpoint. A concurrently started owner is observed and
    // accepted only if it is a fresh instance of the verified replacement.
    crate::connect_or_start_daemon_at_with_executable(
      &self.client.socket_path,
      Some(&self.available.executable),
    )
    .await?;
    let deadline = Instant::now() + QUERY_TIMEOUT;
    loop {
      if let DaemonStatus::Running { info } = self.client.probe().await? {
        if info.binary != self.available.info
          || self
            .before
            .as_ref()
            .is_some_and(|before| before.instance_id == info.instance_id)
        {
          return Err(LifecycleError::VerificationFailed);
        }
        return Ok(RestartOutcome {
          before: self.before,
          after: info,
        });
      }
      if Instant::now() >= deadline {
        return Err(LifecycleError::VerificationFailed);
      }
      sleep(Duration::from_millis(25)).await;
    }
  }
}

fn resolve_executable(selected: &std::path::Path) -> Result<PathBuf, LifecycleError> {
  let path = if selected.components().count() == 1 {
    std::env::var_os("PATH")
      .into_iter()
      .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
      .map(|directory| directory.join(selected))
      .find(|path| path.is_file())
      .ok_or_else(|| LifecycleError::Unavailable("ctld was not found on PATH".into()))?
  } else {
    selected.to_owned()
  };
  path.canonicalize().map_err(Into::into)
}

async fn inspect(stream: &mut crate::Stream) -> Result<DaemonInfo, LifecycleError> {
  timeout(QUERY_TIMEOUT, async {
    crate::write_frame(
      stream,
      &Request::CtldInspect {
        protocol_version: PROTOCOL_VERSION,
      },
    )
    .await?;
    let response = match crate::read_frame::<_, Response>(stream).await {
      Err(crate::CodecError::Json(_)) => return Err(LifecycleError::Unsupported),
      result => result?,
    };
    match response {
      Some(Response::CtldInfo { info }) => Ok(info),
      Some(Response::CtldError { code, message }) => {
        Err(LifecycleError::Rejected { code, message })
      }
      _ => Err(LifecycleError::Unsupported),
    }
  })
  .await
  .map_err(|_| LifecycleError::Timeout)?
}

#[cfg(all(test, unix))]
mod tests;
