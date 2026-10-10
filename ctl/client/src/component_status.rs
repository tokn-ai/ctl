//! Passive observation of one existing local component owner.

use ctl_core::component::{ComponentBuildInfo, LegacyProtocolInfo, ProtocolInfo};
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOwner {
  Ctld,
  Ctmuxd,
  CtlTaskd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerState {
  Absent,
  Legacy,
  Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalObservation {
  pub state: OwnerState,
  pub build: Option<ComponentBuildInfo>,
  pub protocols: Vec<ProtocolInfo>,
  pub legacy_protocols: Vec<LegacyProtocolInfo>,
  pub protocol_mismatch: bool,
  pub restart_supported: bool,
}

impl LocalObservation {
  fn absent() -> Self {
    Self {
      state: OwnerState::Absent,
      build: None,
      protocols: Vec::new(),
      legacy_protocols: Vec::new(),
      protocol_mismatch: false,
      restart_supported: false,
    }
  }
}

/// Observes the selected owner without starting a service, discovering an
/// executable, or preparing a restart. The caller chooses the exact endpoint.
///
/// # Errors
/// Returns transport, timeout, and malformed-response errors from the owner.
pub async fn observe_local(owner: LocalOwner, socket: &Path) -> io::Result<LocalObservation> {
  match owner {
    LocalOwner::Ctld => {
      use ctl_ipc::lifecycle::DaemonStatus;

      match ctl_ipc::lifecycle::Client::new(socket.to_path_buf())
        .probe()
        .await
        .map_err(io::Error::other)?
      {
        DaemonStatus::Absent => Ok(LocalObservation::absent()),
        DaemonStatus::Legacy { protocol_version } => Ok(LocalObservation {
          state: OwnerState::Legacy,
          legacy_protocols: protocol_version
            .map(|version| LegacyProtocolInfo {
              name: "ctld".into(),
              version,
            })
            .into_iter()
            .collect(),
          ..LocalObservation::absent()
        }),
        DaemonStatus::Running { info } => Ok(LocalObservation {
          state: OwnerState::Running,
          build: Some(info.binary.build),
          protocols: info.binary.protocols,
          legacy_protocols: Vec::new(),
          protocol_mismatch: false,
          restart_supported: true,
        }),
      }
    }
    LocalOwner::Ctmuxd => {
      let Some(info) = ctmux_ipc::lifecycle::Client::new(socket.to_path_buf())
        .observe()
        .await
        .map_err(io::Error::other)?
      else {
        return Ok(LocalObservation::absent());
      };
      let state = if info.component_info().is_some() {
        OwnerState::Running
      } else {
        OwnerState::Legacy
      };
      Ok(LocalObservation {
        state,
        build: info.build,
        protocols: info.protocols,
        legacy_protocols: info.legacy_protocols,
        protocol_mismatch: false,
        restart_supported: info.restart_supported,
      })
    }
    LocalOwner::CtlTaskd => {
      let Some(info) = ctl_task_ipc::component_status_at(socket).await? else {
        return Ok(LocalObservation::absent());
      };
      Ok(LocalObservation {
        state: if info.build.is_some() {
          OwnerState::Running
        } else {
          OwnerState::Legacy
        },
        build: info.build,
        protocols: info.protocols,
        legacy_protocols: Vec::new(),
        protocol_mismatch: info.protocol_mismatch,
        restart_supported: !info.protocol_mismatch,
      })
    }
  }
}
