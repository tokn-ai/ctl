//! Saved hosts remain inspectable even when their terminal daemon cannot reply.
use std::{path::PathBuf, time::Duration};

use ctl_client::hosts::{ConnectionTargetDto, HostCatalogDocument};
use ctl_proto::maintenance::{ComponentKind as Kind, ComponentState as State, RemoteComponents};

use super::models::{ComponentAction, ComponentVersionInfo, ComponentVersionRow, VersionStatus};
use crate::error::{CommandErrorDto, CommandResult};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Selection {
  pub host_id: String,
  pub label: String,
  pub target: ConnectionTargetDto,
  pub remote_id: Option<String>,
}

impl Selection {
  pub fn component_id(&self, component: &str) -> String {
    format!("remote:saved:{}:{component}", self.host_id)
  }

  pub async fn master(&self) -> CommandResult<PathBuf> {
    tokio::time::timeout(
      Duration::from_secs(4),
      crate::ssh_auth::existing_master(&self.target),
    )
    .await
    .map_err(|_| {
      CommandErrorDto::new(
        "remote_master_timeout",
        "The existing SSH connection did not respond.",
      )
    })?
  }
}

async fn selections(catalog: &HostCatalogDocument) -> Vec<CommandResult<Selection>> {
  let resolved: Vec<_> = catalog
    .hosts
    .iter()
    .map(|host| ctl_client::hosts::resolve(catalog, &host.host_id, None))
    .collect();
  let devices = if resolved.iter().any(|host| {
    host
      .as_ref()
      .is_ok_and(ctl_client::hosts::ResolvedHost::requires_tailscale)
  }) {
    // Read-only discovery never starts or logs in to Tailscale. Resolve bound
    // devices just as the connection flow does; do not reuse a stale address.
    ctl_client::tailscale::discover_devices().await.devices
  } else {
    Vec::new()
  };
  catalog
    .hosts
    .iter()
    .zip(resolved)
    .map(|(host, resolved)| {
      let mut resolved =
        resolved.map_err(|error| CommandErrorDto::new(error.code, error.message))?;
      resolved
        .resolve_tailscale(&devices)
        .map_err(|error| CommandErrorDto::new(error.code, error.message))?;
      Ok(Selection {
        host_id: host.host_id.clone(),
        label: host.name.clone(),
        target: resolved.target,
        remote_id: host
          .remote_info
          .as_ref()
          .map(|identity| identity.remote_id.clone()),
      })
    })
    .collect()
}

pub(super) async fn selection(app: &tauri::AppHandle, id: &str) -> CommandResult<Selection> {
  let catalog = crate::workspace::load_hosts(app.clone()).await?.document;
  selections(&catalog)
    .await
    .into_iter()
    .filter_map(Result::ok)
    .find(|selected| selected.component_id("ctmuxd") == id)
    .ok_or_else(|| {
      CommandErrorDto::new(
        "remote_component_changed",
        "The saved host or preferred method changed. Refresh Components and check again.",
      )
    })
}

pub(super) async fn rows(
  app: &tauri::AppHandle,
  priority_host: Option<&str>,
) -> CommandResult<Vec<ComponentVersionRow>> {
  let catalog = crate::workspace::load_hosts(app.clone()).await?.document;
  let selections = selections(&catalog).await;
  let mut tasks = tokio::task::JoinSet::new();
  let mut rows = Vec::new();
  // Limit concurrency and the entire refresh, so a large offline catalog
  // cannot hold the page indefinitely. Unchecked hosts remain explicit rows.
  let mut selected: Vec<_> = selections
    .clone()
    .into_iter()
    .filter_map(Result::ok)
    .collect();
  selected.sort_by_key(|selected| priority_host != Some(selected.host_id.as_str()));
  let mut pending = selected.iter();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
  for selected in pending.by_ref().take(4) {
    let selected = selected.clone();
    tasks.spawn(async move { inspect(selected).await });
  }
  while !tasks.is_empty() {
    let Ok(Some(result)) = tokio::time::timeout_at(deadline, tasks.join_next()).await else {
      break;
    };
    rows.extend(result.map_err(CommandErrorDto::backend)?);
    if let Some(selected) = pending.next() {
      let selected = selected.clone();
      tasks.spawn(async move { inspect(selected).await });
    }
  }
  tasks.abort_all();
  for selected in selected {
    if !rows
      .iter()
      .any(|row| row.host_id.as_deref() == Some(&selected.host_id))
    {
      let mut unchecked = empty_rows(&selected);
      for row in &mut unchecked {
        row.detail = Some("Not checked within this refresh. Check host to authenticate and inspect it individually.".into());
      }
      rows.extend(unchecked);
    }
  }
  // Invalid routes still appear as saved hosts, without trying another route.
  for (host, selection) in catalog.hosts.iter().zip(selections) {
    if let Err(error) = selection {
      let mut row = ComponentVersionRow::local("ctl_agent", &host.name);
      row.component_id = format!("remote:saved:{}:ctl_agent", host.host_id);
      row.location = "remote";
      row.host_id = Some(host.host_id.clone());
      row.host_key = Some(format!("saved:{}", host.host_id));
      row.host_name = Some(host.name.clone());
      row.observation = "not_checked";
      row.status = VersionStatus::Unavailable;
      row.error = Some(error.message);
      row.error_code = Some(error.code);
      rows.push(row);
    }
  }
  rows.sort_by(|a, b| a.label.cmp(&b.label));
  Ok(rows)
}

async fn inspect(selected: Selection) -> Vec<ComponentVersionRow> {
  let mut rows = empty_rows(&selected);
  let result = async {
    let master = selected.master().await?;
    for row in &mut rows {
      row.connected = Some(true);
    }
    let remote_id = selected.remote_id.as_deref().ok_or_else(|| {
      CommandErrorDto::new(
        "remote_identity_unverified",
        "Connect and verify this saved host's account before inspecting its components.",
      )
    })?;
    let ctl_client::ConnectionTarget::Ssh {
      destination,
      options,
    } = selected.target.to_core()
    else {
      return Err(CommandErrorDto::new(
        "invalid_ssh_target",
        "Select an SSH host.",
      ));
    };
    // Probe the installed agent, not the metadata cached on an older terminal
    // channel. Published maintenance 1.0.2 cannot inspect companion owners.
    let identity = ctl_client::maintenance::inspect_agent(&destination, &options, &master)
      .await
      .map_err(agent_inspection_error)?;
    apply_agent(&mut rows[0], &selected, identity, remote_id)?;
    ctl_client::maintenance::inspect_components(&destination, &options, &master, remote_id)
      .await
      .map_err(|error| CommandErrorDto::new(error.code, error.message))
  }
  .await;
  match result {
    Ok(snapshot) => apply(&mut rows, snapshot),
    Err(error) => {
      apply_error(&mut rows, &error);
    }
  }
  rows
}

fn inspection_unsupported() -> CommandErrorDto {
  CommandErrorDto::new(
    "remote_component_inspection_unsupported",
    "The installed agent does not support component inspection (maintenance contract 1.1.3). Update this host's components; existing sessions can stay running.",
  )
}

fn agent_inspection_error(error: ctl_client::CoreError) -> CommandErrorDto {
  match &error {
    ctl_client::CoreError::SshCommandFailed { diagnostic, .. }
      if diagnostic.contains("unrecognized subcommand 'inspect'") =>
    {
      inspection_unsupported()
    }
    _ => CommandErrorDto::backend(error),
  }
}

fn apply_agent(
  row: &mut ComponentVersionRow,
  selected: &Selection,
  identity: ctl_proto::RemoteIdentity,
  remote_id: &str,
) -> CommandResult<()> {
  selected.target.verify_remote_identity(&identity)?;
  if identity.remote_id != remote_id {
    return Err(CommandErrorDto::new(
      "remote_identity_mismatch",
      "This connection reports a different remote account. Check the saved host before updating.",
    ));
  }
  let supported = identity.protocols.iter().any(|protocol| {
    protocol.name == "ctl_maintenance"
      && protocol
        .supported_versions
        .contains(&ctl_proto::maintenance::CONTRACT_V1_1_3)
  });
  row.installed = Some(ComponentVersionInfo::observed(
    identity.agent_version,
    identity.build,
    identity.protocols.into_iter().map(Into::into).collect(),
  ));
  row.observation = "installed";
  row.status = VersionStatus::Unknown;
  row.detail = Some("Checked the installed agent through the existing SSH connection.".into());
  if !supported {
    return Err(inspection_unsupported());
  }
  Ok(())
}

fn apply_error(rows: &mut [ComponentVersionRow], error: &CommandErrorDto) {
  for row in rows
    .iter_mut()
    .filter(|row| row.observation == "not_checked")
  {
    if matches!(
      error.code.as_str(),
      "ssh_authentication_required" | "ssh_host_disconnected"
    ) {
      row.connected = Some(false);
    }
    row.detail = Some("Refresh reuses an existing SSH connection without starting services. Check host can authenticate and inspect this host.".into());
    row.error = Some(error.message.clone());
    row.error_code = Some(error.code.clone());
  }
}

fn empty_rows(selected: &Selection) -> Vec<ComponentVersionRow> {
  [
    ("ctl_agent", "ctl-agent"),
    ("ctld", "ctld"),
    ("ctmuxd", "ctmuxd"),
    ("ctl-taskd", "ctl-taskd"),
  ]
  .into_iter()
  .map(|(component, label)| {
    let mut row = ComponentVersionRow::local(component, &format!("{label} — {}", selected.label));
    row.component_id = selected.component_id(component);
    row.location = "remote";
    row.host_id = Some(selected.host_id.clone());
    row.host_key = Some(format!("saved:{}", selected.host_id));
    row.host_name = Some(selected.label.clone());
    row.observation = "not_checked";
    row.status = VersionStatus::Unavailable;
    row
  })
  .collect()
}

fn apply(rows: &mut [ComponentVersionRow], snapshot: RemoteComponents) {
  for observed in snapshot.components {
    let component = match observed.component {
      Kind::CtlAgent => "ctl_agent",
      Kind::Ctld => "ctld",
      Kind::Ctmuxd => "ctmuxd",
      Kind::CtlTaskd => "ctl-taskd",
    };
    let Some(row) = rows.iter_mut().find(|row| row.component == component) else {
      continue;
    };
    row.installed = observed.installed.map(ComponentVersionInfo::from_component);
    row.available = row.installed.clone();
    row.running = observed.running.map(ComponentVersionInfo::from_component);
    row.legacy_protocols = observed.legacy_protocols;
    row.observation = if observed.state == State::Legacy {
      "legacy"
    } else {
      "running"
    };
    row.error = observed.error;
    row.compare();
    row.status = match observed.state {
      State::Running => row.status,
      State::NotRunning => VersionStatus::NotRunning,
      State::Legacy => VersionStatus::Incompatible,
      State::Unavailable => VersionStatus::Unavailable,
      State::OnDemand => {
        row.observation = "installed";
        VersionStatus::Unknown
      }
    };
    row.restart_supported = component == "ctmuxd" && observed.restart_supported && compatible(row);
    row.action = row.restart_supported.then_some(ComponentAction::Restart);
    row.detail = Some(match observed.state {
      State::OnDemand => "An agent starts for each SSH channel. Existing terminal transports keep their running agent until reconnected.",
      State::Legacy if component != "ctmuxd" => "This owner predates published contracts. Installing components preserves it. Restart this service on the host after its work can be ended.",
      State::Legacy => "This owner uses historical numeric protocols. Installing components preserves it; a confirmed restart applies the installed daemon.",
      State::Unavailable => "Could not inspect the running owner. Installing components preserves running sessions.",
      _ if row.restart_required && component != "ctmuxd" => "Installed and running builds differ. Restart this service on the host after its work can be ended.",
      _ if row.restart_required => "Installed and running builds differ. Installing components preserves the running daemon; restart separately to apply the installed build.",
      _ => "Checked through the existing SSH connection without opening a terminal or starting services.",
    }.into());
  }
}

fn compatible(row: &ComponentVersionRow) -> bool {
  row.installed.as_ref().is_some_and(|installed| {
    row.required_protocols.iter().all(|required| {
      installed.protocols.iter().any(|p| {
        p.name == required.name
          && p
            .supported_versions
            .iter()
            .any(|v| required.supported_versions.contains(v))
      })
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn selected() -> Selection {
    Selection {
      host_id: "saved".into(),
      label: "Saved host".into(),
      target: ConnectionTargetDto::ssh("saved"),
      remote_id: Some("account".into()),
    }
  }

  fn agent(inspection_supported: bool) -> ctl_proto::RemoteIdentity {
    let mut protocols = ctl_proto::agent_protocols();
    if !inspection_supported {
      let maintenance = protocols
        .iter_mut()
        .find(|protocol| protocol.name == "ctl_maintenance")
        .unwrap();
      *maintenance = ctl_core::component::ProtocolInfo::new(
        "ctl_maintenance",
        2,
        ctl_proto::maintenance::CONTRACT_V1_0_2,
        &[ctl_proto::maintenance::CONTRACT_V1_0_2],
      );
    }
    ctl_proto::RemoteIdentity {
      remote_id: "account".into(),
      agent_version: "0.1.0".into(),
      build: Some(ctl_core::component::build_info()),
      ctmux_restart_supported: true,
      bundle: None,
      protocols,
    }
  }

  #[test]
  fn old_installed_agents_stay_checked_while_companion_inspection_needs_an_update() {
    let selected = selected();
    let mut rows = empty_rows(&selected);
    for row in &mut rows {
      row.connected = Some(true);
    }
    let error = apply_agent(&mut rows[0], &selected, agent(false), "account").unwrap_err();
    assert_eq!(error.code, "remote_component_inspection_unsupported");
    apply_error(&mut rows, &error);
    assert_eq!(rows[0].observation, "installed");
    assert!(rows[0].installed.is_some());
    assert!(rows[0].error.is_none());
    assert!(rows[1..].iter().all(|row| row.connected == Some(true)
      && row.error_code.as_deref() == Some("remote_component_inspection_unsupported")));
    // An already installed compatible replacement permits inspection, even
    // when the terminal's own agent still reports a previous build.
    assert!(apply_agent(&mut rows[0], &selected, agent(true), "account").is_ok());
  }

  #[test]
  fn an_unsupported_inspect_command_is_distinct_from_an_authentication_failure() {
    for (diagnostic, code) in [
      (
        "error: unrecognized subcommand 'inspect'",
        "remote_component_inspection_unsupported",
      ),
      ("Permission denied (publickey).", "backend_error"),
    ] {
      let error = agent_inspection_error(ctl_client::CoreError::SshCommandFailed {
        status: "exit status: 2".into(),
        diagnostic: diagnostic.into(),
      });
      assert_eq!(error.code, code);
    }
  }

  #[test]
  fn changed_remote_identity_cannot_become_a_checked_installation() {
    let selected = selected();
    let mut rows = empty_rows(&selected);
    let error = apply_agent(&mut rows[0], &selected, agent(true), "different-account").unwrap_err();
    assert_eq!(error.code, "remote_identity_mismatch");
    assert!(rows[0].installed.is_none());
    assert_eq!(rows[0].observation, "not_checked");
  }

  #[test]
  fn missing_authentication_marks_unchecked_hosts_as_disconnected() {
    let mut rows = empty_rows(&selected());
    apply_error(
      &mut rows,
      &CommandErrorDto::new("ssh_authentication_required", "Authenticate SSH."),
    );
    assert!(rows.iter().all(|row| row.connected == Some(false)));
    assert!(
      rows
        .iter()
        .all(|row| row.observation == "not_checked" && row.installed.is_none())
    );
  }

  #[test]
  fn absent_and_legacy_owners_remain_distinct_from_installed_binaries() {
    let selected = Selection {
      host_id: "saved".into(),
      label: "Saved host".into(),
      target: ConnectionTargetDto::ssh("saved"),
      remote_id: Some("identity".into()),
    };
    let mut rows = empty_rows(&selected);
    assert!(
      rows
        .iter()
        .all(|row| row.host_key.as_deref() == Some("saved:saved"))
    );
    assert!(
      rows
        .iter()
        .all(|row| row.host_name.as_deref() == Some("Saved host"))
    );
    let installed = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![
        ctmux_proto::protocol_info(),
        ctmux_ipc::local_control_protocol_info(),
      ],
    };
    apply(
      &mut rows,
      RemoteComponents {
        remote_id: "identity".into(),
        components: vec![ctl_proto::maintenance::RemoteComponent {
          component: Kind::Ctmuxd,
          installed: Some(installed),
          running: None,
          state: State::Legacy,
          restart_supported: true,
          legacy_protocols: vec![ctl_core::component::LegacyProtocolInfo {
            name: "ctmux_control".into(),
            version: 1,
          }],
          error: None,
        }],
      },
    );
    let row = &rows[2];
    assert!(row.installed.is_some());
    assert!(row.running.is_none());
    assert!(row.restart_required);
    assert_eq!(row.action, Some(ComponentAction::Restart));
    assert!(rows[0].installed.is_none());
    assert_eq!(rows[0].observation, "not_checked");
  }
}
