use super::{Daemon, build_label, compatible, flag, output, remote, validate_target};
use ctl_client::component_status::{LocalOwner, OwnerState, observe_local};
use ctl_core::component::ComponentInfo;
use ctl_core::component::{LegacyProtocolInfo, ProtocolInfo, protocols_match};
use serde::Serialize;
use std::io;
use std::path::Path;

#[derive(Serialize)]
struct Row {
  component: String,
  state: String,
  running: Option<ComponentInfo>,
  running_protocols: Vec<ProtocolInfo>,
  legacy_protocols: Vec<LegacyProtocolInfo>,
  protocol_mismatch: bool,
  available: Option<ComponentInfo>,
  available_missing: bool,
  running_compatible: Option<bool>,
  available_compatible: Option<bool>,
  restart_needed: Option<bool>,
  restart_supported: bool,
  warnings: Vec<String>,
  errors: Vec<String>,
}

impl Row {
  fn new(component: &str) -> Self {
    Self {
      component: component.into(),
      state: "unknown".into(),
      running: None,
      running_protocols: Vec::new(),
      legacy_protocols: Vec::new(),
      protocol_mismatch: false,
      available: None,
      available_missing: false,
      running_compatible: None,
      available_compatible: None,
      restart_needed: None,
      restart_supported: false,
      warnings: Vec::new(),
      errors: Vec::new(),
    }
  }

  fn set_available(&mut self, available: io::Result<Option<ComponentInfo>>) {
    match available {
      Ok(Some(info)) => self.available = Some(info),
      Ok(None) => {
        self.available_missing = true;
        self.warnings.push(format!("No {} replacement was found by automatic discovery. Build/install it beside this CLI or select a local bundle before restarting.", self.component));
      }
      Err(error) => self.errors.push(format!("Installed replacement: {error}")),
    }
  }

  fn finish(&mut self, component: Daemon) {
    if let Some(info) = &self.running {
      self.running_protocols.clone_from(&info.protocols);
    }
    self.running_compatible = self
      .running
      .as_ref()
      .map(|info| compatible(component, info));
    if self.running_compatible.is_none() && !self.running_protocols.is_empty() {
      self.running_compatible = Some(component.required().iter().all(|(name, versions)| {
        self
          .running_protocols
          .iter()
          .any(|protocol| protocol.name == *name && protocol.negotiate(versions).is_some())
      }));
    }
    if self.protocol_mismatch {
      self.running_compatible = Some(false);
    }
    self.available_compatible = self
      .available
      .as_ref()
      .map(|info| compatible(component, info));
    self.restart_needed =
      self
        .running
        .as_ref()
        .zip(self.available.as_ref())
        .map(|(running, available)| {
          running.build != available.build
            || !protocols_match(&running.protocols, &available.protocols)
        });
  }
}

async fn observe(component: Daemon) -> io::Result<Row> {
  let (owner, socket) = match component {
    Daemon::Ctld => (LocalOwner::Ctld, ctl_ipc::socket_path()),
    Daemon::Ctmuxd => (LocalOwner::Ctmuxd, ctmux_ipc::socket_path()),
    Daemon::CtlTaskd => (LocalOwner::CtlTaskd, ctl_task_ipc::socket_path()),
  };
  let observation = observe_local(owner, &socket).await?;
  let mut row = Row::new(component.name());
  row.state = match observation.state {
    OwnerState::Absent => "not_running",
    OwnerState::Legacy => "legacy",
    OwnerState::Running => "running",
  }
  .into();
  row.running_protocols.clone_from(&observation.protocols);
  row.running = observation
    .build
    .filter(|_| observation.state == OwnerState::Running)
    .map(|build| ComponentInfo {
      build,
      protocols: observation.protocols,
    });
  row.legacy_protocols = observation.legacy_protocols;
  row.protocol_mismatch = observation.protocol_mismatch;
  row.restart_supported = observation.restart_supported;
  Ok(row)
}

// A bare default name that is absent on PATH is a normal installation state.
// Missing explicit overrides and damaged managed/absolute selections remain errors.
fn classify_available(
  result: io::Result<ComponentInfo>,
  selected: &Path,
  explicit: bool,
) -> io::Result<Option<ComponentInfo>> {
  match result {
    Err(error)
      if !explicit
        && selected.components().count() == 1
        && error.kind() == io::ErrorKind::NotFound =>
    {
      Ok(None)
    }
    result => result.map(Some),
  }
}

async fn local_row(component: Daemon) -> Row {
  // Discovery and metadata inspection do not call the bundled installation provider.
  let available = async {
    let selected = component.executable()?;
    classify_available(
      ctl_core::executable::inspect(&selected).await,
      &selected,
      component.has_executable_override(),
    )
  };
  let (running, available) = tokio::join!(observe(component), available);
  let mut row = running.unwrap_or_else(|error| {
    let mut row = Row::new(component.name());
    row.errors.push(format!("Running owner: {error}"));
    row
  });
  row.set_available(available);
  row.finish(component);
  row
}

fn remote_row(component: ctl_proto::maintenance::RemoteComponent) -> Row {
  use ctl_proto::maintenance::{ComponentKind, ComponentState};
  let (name, daemon) = match component.component {
    ComponentKind::CtlAgent => ("ctl-agent", None),
    ComponentKind::Ctld => ("ctld", Some(Daemon::Ctld)),
    ComponentKind::Ctmuxd => ("ctmuxd", Some(Daemon::Ctmuxd)),
    ComponentKind::CtlTaskd => ("ctl-taskd", Some(Daemon::CtlTaskd)),
  };
  let mut row = Row::new(name);
  row.state = match component.state {
    ComponentState::Running => "running",
    ComponentState::NotRunning => "not_running",
    ComponentState::Legacy => "legacy",
    ComponentState::Unavailable => "unavailable",
    ComponentState::OnDemand => "on_demand",
  }
  .into();
  row.running = component.running;
  row.available = component.installed;
  row.restart_supported = daemon == Some(Daemon::Ctmuxd) && component.restart_supported;
  row.errors.extend(component.error);
  row.legacy_protocols = component.legacy_protocols;
  if let Some(daemon) = daemon {
    row.finish(daemon);
  } else {
    row.available_compatible = row.available.as_ref().map(agent_compatible);
  }
  row
}

fn agent_compatible(info: &ComponentInfo) -> bool {
  let required = [
    (
      "ctl_identity",
      ctl_proto::IDENTITY_SUPPORTED_PROTOCOL_VERSIONS,
    ),
    (
      "ctl_maintenance",
      ctl_proto::maintenance::SUPPORTED_PROTOCOL_VERSIONS,
    ),
    (
      "ctl_remote_vpn",
      ctl_ipc::remote_vpn::SUPPORTED_PROTOCOL_VERSIONS,
    ),
    ("ctld", ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS),
  ];
  info.is_valid()
    && required
      .iter()
      .chain(Daemon::Ctmuxd.required())
      .chain(Daemon::CtlTaskd.required())
      .all(|(name, versions)| {
        info
          .protocols
          .iter()
          .any(|protocol| protocol.name == *name && protocol.negotiate(versions).is_some())
      })
}

pub(crate) async fn run(
  host: Option<&str>,
  method: Option<&str>,
  platform: Option<crate::RemotePlatform>,
  json: bool,
) -> io::Result<()> {
  validate_target(host, method, platform, None)?;
  let mut remote_id = None;
  let rows = if let Some(host) = host {
    let remote = remote(host, method).await?;
    let snapshot = ctl_client::maintenance::inspect_components(
      &remote.destination,
      &remote.options,
      &remote.control_path,
      &remote.remote_id,
    )
    .await
    .map_err(io::Error::other)?;
    remote_id = Some(snapshot.remote_id);
    snapshot
      .components
      .into_iter()
      .map(remote_row)
      .collect::<Vec<_>>()
  } else {
    let (ctld, ctmuxd, taskd) = tokio::join!(
      local_row(Daemon::Ctld),
      local_row(Daemon::Ctmuxd),
      local_row(Daemon::CtlTaskd)
    );
    vec![ctld, ctmuxd, taskd]
  };
  if json {
    output(
      &serde_json::json!({ "host": host.unwrap_or("local"), "remote_id": remote_id, "components": rows }),
    )?;
  } else {
    let table = crate::table::format(
      [
        "Component",
        "State",
        "Running build",
        "Available build",
        "Compatible¹",
        "Restart needed",
      ],
      rows.iter().map(|row| {
        [
          row.component.clone(),
          row.state.clone(),
          build_label(row.running.as_ref()),
          if row.available_missing {
            "not installed".into()
          } else {
            build_label(row.available.as_ref())
          },
          format!(
            "{} / {}",
            flag(row.running_compatible),
            flag(row.available_compatible)
          ),
          flag(row.restart_needed).into(),
        ]
      }),
    );
    println!(
      "{table}\n¹ Running / available protocol compatibility; unknown metadata is shown as ?.\nStatus does not start or restart the inspected services."
    );
    for row in &rows {
      for warning in &row.warnings {
        eprintln!("{}: {}", row.component, crate::table::text(warning));
      }
      for error in &row.errors {
        eprintln!("{}: {}", row.component, crate::table::text(error));
      }
    }
  }
  if rows.iter().any(|row| !row.errors.is_empty()) {
    return Err(io::Error::other("some components could not be inspected"));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::{Daemon, LegacyProtocolInfo, Row, classify_available, remote_row};
  use ctl_core::component::ComponentInfo;
  use std::{io, path::Path};

  #[test]
  fn restart_comparison_uses_full_build_and_contracts() {
    let info = ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![
        ctl_task_proto::protocol_info(),
        ctl_task_proto::control::protocol_info(),
      ],
    };
    let mut row = Row::new("ctl-taskd");
    row.running = Some(info.clone());
    row.available = Some(info.clone());
    row.finish(Daemon::CtlTaskd);
    assert_eq!(row.restart_needed, Some(false));
    assert_eq!(row.available_compatible, Some(true));
    row.available.as_mut().unwrap().build.source_fingerprint = "a".repeat(64);
    row.finish(Daemon::CtlTaskd);
    assert_eq!(row.restart_needed, Some(true));
    row
      .available
      .as_mut()
      .unwrap()
      .protocols
      .retain(|protocol| protocol.name != "task_control");
    row.finish(Daemon::CtlTaskd);
    assert_eq!(row.available_compatible, Some(false));
    row.running = None;
    row.finish(Daemon::CtlTaskd);
    assert_eq!(row.restart_needed, None);
  }

  #[test]
  fn legacy_remote_owner_keeps_its_protocol_metadata_without_inventing_a_build() {
    use ctl_proto::maintenance::{ComponentKind, ComponentState, RemoteComponent};
    let legacy = vec![LegacyProtocolInfo {
      name: "ctmux_control".into(),
      version: 1,
    }];
    let row = remote_row(RemoteComponent {
      component: ComponentKind::Ctmuxd,
      state: ComponentState::Legacy,
      installed: None,
      running: None,
      restart_supported: true,
      legacy_protocols: legacy.clone(),
      error: None,
    });
    assert_eq!(row.legacy_protocols, legacy);
    assert_eq!(row.restart_needed, None);
    assert_eq!(row.running_compatible, None);
    assert!(row.restart_supported);
  }

  #[test]
  fn typed_protocol_rejection_is_preserved_without_build_metadata() {
    let mut row = Row::new("ctl-taskd");
    row.protocol_mismatch = true;
    row.finish(Daemon::CtlTaskd);
    assert_eq!(row.running_compatible, Some(false));
    assert_eq!(row.restart_needed, None);
  }

  #[test]
  fn missing_automatic_replacement_is_nonfatal_but_explicit_selections_are_errors() {
    let missing = || Err(io::Error::new(io::ErrorKind::NotFound, "missing helper"));
    let mut row = Row::new("ctmuxd");
    row.set_available(classify_available(missing(), Path::new("ctmuxd"), false));
    assert!(row.available_missing);
    assert_eq!(row.errors, [] as [String; 0]);
    assert_eq!(row.warnings.len(), 1);
    assert!(classify_available(missing(), Path::new("ctmuxd"), true).is_err());
    assert!(classify_available(missing(), Path::new("/selected/ctmuxd"), false).is_err());
    assert!(
      classify_available(
        Err(io::Error::other("invalid metadata")),
        Path::new("ctmuxd"),
        false
      )
      .is_err()
    );
  }
}
