//! Confirmed actions bound to active, app-owned remote transports.
pub(crate) mod reconnect;

use std::sync::Arc;

use tauri::Emitter as _;

use super::{
  models::{ComponentAction, ComponentVersionInfo, ComponentVersionRow, ProtocolVersion},
  observations,
};
use crate::{
  error::{CommandErrorDto, CommandResult},
  state::{AppState, AttachmentActor},
};

pub struct RemoteActionPreview {
  pub component_id: String,
  pub label: String,
  pub component: &'static str,
  pub host_id: Option<String>,
  pub action: ComponentAction,
  pub running: Option<ComponentVersionInfo>,
  pub available: Option<ComponentVersionInfo>,
  pub attachment_count: u32,
  pub detail: String,
}

pub struct PreparedRemoteAction {
  pub preview: RemoteActionPreview,
  actors: Vec<Arc<AttachmentActor>>,
  remote_id: String,
  operation: Operation,
  selection: Option<super::inventory::Selection>,
}

enum Operation {
  Restart(Box<ctl_client::maintenance::PreparedRemoteCtmuxRestart>),
  Reconnect,
}

pub struct RemoteActionOutcome {
  pub running: Option<ComponentVersionInfo>,
  pub detail: String,
}

pub async fn prepare(
  app: &tauri::AppHandle,
  state: &AppState,
  component_id: &str,
) -> CommandResult<PreparedRemoteAction> {
  if component_id.starts_with("remote:saved:") {
    return prepare_saved(app, state, component_id).await;
  }
  let active = state.remote_actors().await;
  let actors: Vec<_> = active
    .into_iter()
    .filter(|actor| row(actor, component_id).is_some())
    .collect();
  let actor = actors.first().ok_or_else(unavailable)?;
  let row = row(actor, component_id).ok_or_else(unavailable)?;
  let observation = actor.remote_observation.as_ref().ok_or_else(unavailable)?;
  let remote_id = observation.identity.remote_id.clone();
  let control_path = tokio::time::timeout(
    std::time::Duration::from_secs(5),
    crate::ssh_auth::existing_master(&actor.target),
  )
  .await
  .map_err(|_| {
    CommandErrorDto::new(
      "remote_master_timeout",
      "The existing SSH connection did not respond. Nothing was restarted.",
    )
  })??;
  let ctl_client::ConnectionTarget::Ssh {
    destination,
    options,
  } = actor.target.to_core()
  else {
    return Err(unavailable());
  };
  let mut preview = RemoteActionPreview {
    component_id: component_id.into(),
    label: row.label,
    component: row.component,
    host_id: row.host_id,
    action: row.action.ok_or_else(unavailable)?,
    running: row.running,
    available: None,
    attachment_count: u32::try_from(actors.len()).unwrap_or(u32::MAX),
    detail: String::new(),
  };
  let operation = if preview.component == "ctmuxd" {
    let prepared = ctl_client::maintenance::prepare_ctmux_restart(
      &destination,
      &options,
      &control_path,
      &remote_id,
    )
    .await
    .map_err(|error| CommandErrorDto::new(error.code, error.message))?;
    require_compatible_replacement(&prepared.info.available)?;
    let running = &prepared.info.running;
    preview.running = Some(match &running.build {
      Some(build) => ComponentVersionInfo::from_build(build.clone(), running_protocols(running)),
      None => ComponentVersionInfo {
        protocols: running_protocols(running),
        ..ComponentVersionInfo::default()
      },
    });
    preview.available = Some(version(prepared.info.available.clone()));
    preview.detail = "Ends every terminal session owned by this remote account, including sessions in other windows and clients. The installed remote daemon restarts with its default runtime options; no component installation is performed.".into();
    Operation::Restart(Box::new(prepared))
  } else {
    if let Ok(identity) =
      ctl_client::maintenance::inspect_agent(&destination, &options, &control_path).await
    {
      if identity.remote_id != remote_id {
        return Err(unavailable());
      }
      preview.available = Some(agent_version(&identity));
    }
    preview.detail = "Reconnects the selected ctl-agent transports in every app window. Existing remote terminal sessions and their processes remain running.".into();
    if preview.available.is_none() {
      preview.detail.push_str(" This installed agent cannot report its replacement build before reconnect; the new connection will verify identity and report the actual version.");
    }
    Operation::Reconnect
  };
  Ok(PreparedRemoteAction {
    preview,
    actors,
    remote_id,
    operation,
    selection: None,
  })
}

async fn prepare_saved(
  app: &tauri::AppHandle,
  state: &AppState,
  component_id: &str,
) -> CommandResult<PreparedRemoteAction> {
  let selected = super::inventory::selection(app, component_id).await?;
  let remote_id = selected.remote_id.clone().ok_or_else(unavailable)?;
  let master = selected.master().await?;
  let ctl_client::ConnectionTarget::Ssh {
    destination,
    options,
  } = selected.target.to_core()
  else {
    return Err(unavailable());
  };
  let prepared =
    ctl_client::maintenance::prepare_ctmux_restart(&destination, &options, &master, &remote_id)
      .await
      .map_err(|error| CommandErrorDto::new(error.code, error.message))?;
  require_compatible_replacement(&prepared.info.available)?;
  let running = &prepared.info.running;
  let running = match &running.build {
    Some(build) => ComponentVersionInfo::from_build(build.clone(), running_protocols(running)),
    None => ComponentVersionInfo {
      protocols: running_protocols(running),
      ..ComponentVersionInfo::default()
    },
  };
  let actors: Vec<_> = state
    .remote_actors()
    .await
    .into_iter()
    .filter(|actor| {
      actor
        .remote_observation
        .as_ref()
        .is_some_and(|info| info.identity.remote_id == remote_id)
    })
    .collect();
  Ok(PreparedRemoteAction {
    preview: RemoteActionPreview {
      component_id: component_id.into(), label: format!("ctmuxd — {}", selected.label), component: "ctmuxd", host_id: Some(selected.host_id.clone()), action: ComponentAction::Restart,
      running: Some(running), available: Some(version(prepared.info.available.clone())), attachment_count: u32::try_from(actors.len()).unwrap_or(u32::MAX),
      detail: "Ends every terminal session owned by this remote account, including sessions in other windows and clients. Applies the installed daemon without installing components.".into(),
    },
    actors, remote_id, operation: Operation::Restart(Box::new(prepared)), selection: Some(selected),
  })
}

impl PreparedRemoteAction {
  pub async fn execute(
    self,
    app: &tauri::AppHandle,
    state: &AppState,
  ) -> CommandResult<RemoteActionOutcome> {
    if let Some(selected) = &self.selection
      && super::inventory::selection(app, &self.preview.component_id).await? != *selected
    {
      return Err(unavailable());
    }
    let active = state.remote_actors().await;
    if !self
      .actors
      .iter()
      .all(|expected| active.iter().any(|actual| Arc::ptr_eq(expected, actual)))
    {
      return Err(unavailable());
    }
    match self.operation {
      Operation::Reconnect => reconnect::execute(app, &self.actors).await,
      Operation::Restart(prepared) => {
        let environment: Vec<_> = active
          .into_iter()
          .filter(|actor| {
            actor
              .remote_observation
              .as_ref()
              .is_some_and(|observation| observation.identity.remote_id == self.remote_id)
          })
          .collect();
        match prepared.restart().await {
          Ok(result) => {
            reset_sessions(app, &environment, &self.remote_id)?;
            Ok(RemoteActionOutcome {
              running: Some(version(result.after)),
              detail: format!(
                "Restarted the remote daemon and verified its replacement build. Ended {} terminal session(s).",
                result.terminated_sessions
              ),
            })
          }
          Err(error) => {
            if error.may_have_stopped {
              let _ = reset_sessions(app, &environment, &self.remote_id);
            }
            Err(CommandErrorDto::new(error.code, error.message))
          }
        }
      }
    }
  }
}

fn row(actor: &AttachmentActor, component_id: &str) -> Option<ComponentVersionRow> {
  observations::rows(vec![actor.remote_observation.clone()?])
    .into_iter()
    .find(|row| row.component_id == component_id)
}

fn unavailable() -> CommandErrorDto {
  CommandErrorDto::new(
    "remote_component_changed",
    "This remote component is no longer represented by the same active connections. Refresh About and confirm again.",
  )
}

fn running_protocols(info: &ctl_proto::maintenance::RunningCtmux) -> Vec<ProtocolVersion> {
  info
    .protocols
    .clone()
    .into_iter()
    .map(ProtocolVersion::from)
    .collect()
}

fn version(info: ctl_core::component::ComponentInfo) -> ComponentVersionInfo {
  ComponentVersionInfo::from_component(info)
}

fn require_compatible_replacement(info: &ctl_core::component::ComponentInfo) -> CommandResult<()> {
  if !info.is_valid() {
    return Err(CommandErrorDto::new(
      "remote_replacement_incompatible",
      "The installed remote daemon reports invalid protocol metadata.",
    ));
  }
  for (name, supported) in [
    ("ctmux", ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS),
    (
      "ctmux_control",
      ctmux_ipc::LOCAL_CONTROL_SUPPORTED_PROTOCOL_VERSIONS,
    ),
  ] {
    if !info
      .protocols
      .iter()
      .any(|protocol| protocol.name == name && protocol.negotiate(supported).is_some())
    {
      return Err(CommandErrorDto::new(
        "remote_replacement_incompatible",
        "The installed remote daemon uses a protocol incompatible with this app. Update remote components before restarting; nothing was stopped.",
      ));
    }
  }
  Ok(())
}

fn agent_version(identity: &ctl_proto::RemoteIdentity) -> ComponentVersionInfo {
  ComponentVersionInfo::observed(
    identity.agent_version.clone(),
    identity.build.clone(),
    identity
      .protocols
      .clone()
      .into_iter()
      .map(ProtocolVersion::from)
      .collect(),
  )
}

#[derive(Clone, Default, serde::Serialize)]
struct ResetSessions {
  scope: &'static str,
  remote_id: String,
  host_ids: Vec<String>,
  session_ids: Vec<String>,
  attachment_ids: Vec<String>,
}

fn reset_sessions(
  app: &tauri::AppHandle,
  actors: &[Arc<AttachmentActor>],
  remote_id: &str,
) -> CommandResult<()> {
  let mut event = ResetSessions {
    scope: "remote",
    remote_id: remote_id.into(),
    ..ResetSessions::default()
  };
  for actor in actors {
    event.attachment_ids.push(actor.attachment_id.clone());
    if let Some(host_id) = actor
      .remote_observation
      .as_ref()
      .and_then(|info| info.host_id.as_ref())
    {
      event.host_ids.push(host_id.clone());
    }
    if let Some(cache) = &actor.cache_identity {
      event.session_ids.push(cache.session_id.clone());
    }
  }
  event.host_ids.sort();
  event.host_ids.dedup();
  event.session_ids.sort();
  event.session_ids.dedup();
  app.emit("about-reset-sessions", event).map_err(|error| {
    CommandErrorDto::new(
      "remote_restart_refresh_failed",
      format!("The daemon was restarted, but the app could not refresh: {error}"),
    )
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn absent_rows_cannot_choose_an_arbitrary_host_or_command() {
    for id in ["remote:ctmuxd:invented", "ssh:anything", "/tmp/owner.sock"] {
      assert!(
        observations::rows(Vec::new())
          .into_iter()
          .all(|row| row.component_id != id)
      );
    }
  }

  #[test]
  fn incompatible_remote_replacements_are_rejected_before_confirmation() {
    let mut info = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![
        ctmux_proto::protocol_info(),
        ctmux_ipc::local_control_protocol_info(),
      ],
    };
    assert!(require_compatible_replacement(&info).is_ok());
    let future = ctl_core::protocol::ProtocolVersion {
      build: ctmux_proto::PROTOCOL_BUILD + 1,
      ..ctmux_proto::PROTOCOL_VERSION
    };
    info.protocols[0] =
      ctl_core::component::ProtocolInfo::new("ctmux", future.build, future, &[future]);
    assert!(require_compatible_replacement(&info).is_err());
  }
}
