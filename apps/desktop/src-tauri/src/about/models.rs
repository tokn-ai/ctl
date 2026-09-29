use component_info::ComponentBuildInfo;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtocolVersion {
  pub name: String,
  pub version: u16,
}

impl ProtocolVersion {
  pub fn new(name: &str, version: u16) -> Self {
    Self {
      name: name.into(),
      version,
    }
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ComponentVersionInfo {
  pub version: Option<String>,
  pub source_revision: Option<String>,
  pub source_fingerprint: Option<String>,
  pub dirty: Option<bool>,
  pub protocols: Vec<ProtocolVersion>,
}

impl ComponentVersionInfo {
  pub fn from_component(info: component_info::ComponentInfo) -> Self {
    Self::from_build(
      info.build,
      info
        .protocols
        .into_iter()
        .map(|protocol| ProtocolVersion {
          name: protocol.name,
          version: protocol.version,
        })
        .collect(),
    )
  }

  pub fn from_build(build: ComponentBuildInfo, protocols: Vec<ProtocolVersion>) -> Self {
    Self {
      version: Some(build.version),
      source_revision: build.source_revision,
      source_fingerprint: Some(build.source_fingerprint),
      dirty: Some(build.dirty),
      protocols,
    }
  }

  pub fn observed(
    version: String,
    build: Option<ComponentBuildInfo>,
    protocols: Vec<ProtocolVersion>,
  ) -> Self {
    match build {
      Some(build) => Self::from_build(build, protocols),
      None => Self {
        version: Some(version),
        protocols,
        ..Self::default()
      },
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
  Current,
  Outdated,
  Newer,
  DifferentBuild,
  Incompatible,
  Unknown,
  NotRunning,
  Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentAction {
  Restart,
  Reconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentVersionRow {
  pub component_id: String,
  pub component: &'static str,
  pub label: String,
  pub location: &'static str,
  pub host_id: Option<String>,
  pub observation: &'static str,
  pub status: VersionStatus,
  pub running: Option<ComponentVersionInfo>,
  pub available: Option<ComponentVersionInfo>,
  pub required_protocols: Vec<ProtocolVersion>,
  pub restart_supported: bool,
  pub action: Option<ComponentAction>,
  pub detail: Option<String>,
  pub error: Option<String>,
}

impl ComponentVersionRow {
  pub fn local(component: &'static str, label: &str) -> Self {
    Self {
      component_id: component.into(),
      component,
      label: label.into(),
      location: "local",
      host_id: None,
      observation: "running",
      status: VersionStatus::Unknown,
      running: None,
      available: None,
      required_protocols: required_protocols(component),
      restart_supported: false,
      action: None,
      detail: None,
      error: None,
    }
  }

  pub fn compare(&mut self) {
    self.status = compare(self.running.as_ref(), Some(&expected_component_version()));
    if self.running.as_ref().is_some_and(|running| {
      running.protocols.iter().any(|protocol| {
        self
          .required_protocols
          .iter()
          .any(|expected| expected.name == protocol.name && expected.version != protocol.version)
      })
    }) {
      self.status = VersionStatus::Incompatible;
    }
  }

  pub fn note_available_mismatch(&mut self) {
    if matches!(
      compare(self.available.as_ref(), Some(&expected_component_version())),
      VersionStatus::Outdated | VersionStatus::Newer | VersionStatus::DifferentBuild
    ) {
      let explanation = "The selected helper differs from this app's component build. Restarting this helper will not align it with the app; update or rebuild the helper with the app.";
      self.detail = Some(match self.detail.take() {
        Some(previous) => format!("{previous} {explanation}"),
        None => explanation.into(),
      });
    }
  }

  pub fn note_unreported_build(&mut self) {
    if self.status == VersionStatus::Unknown && self.running.is_some() {
      let explanation = "This process does not report enough build information to compare it with the app. Reported versions and protocols are shown below.";
      self.detail = Some(match self.detail.take() {
        Some(previous) => format!("{previous} {explanation}"),
        None => explanation.into(),
      });
    }
  }
}

fn expected_component_version() -> ComponentVersionInfo {
  ComponentVersionInfo::from_build(component_info::build_info(), Vec::new())
}

fn required_protocols(component: &str) -> Vec<ProtocolVersion> {
  match component {
    "ctld" => vec![
      ProtocolVersion::new("ctld", ctld_ipc::PROTOCOL_VERSION),
      ProtocolVersion::new("ctld_lifecycle", ctld_ipc::lifecycle::PROTOCOL_VERSION),
    ],
    "rmuxd" => vec![
      ProtocolVersion::new("rmux", rmux_proto::PROTOCOL_VERSION),
      ProtocolVersion::new("rmux_control", rmux_ipc::LOCAL_CONTROL_PROTOCOL_VERSION),
    ],
    "taskd" => vec![
      ProtocolVersion::new("task", task_proto::PROTOCOL_VERSION),
      ProtocolVersion::new("task_control", task_proto::control::PROTOCOL_VERSION),
    ],
    _ => Vec::new(),
  }
}

#[derive(Debug, Serialize)]
pub struct ComponentVersionsSnapshot {
  pub components: Vec<ComponentVersionRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightRestartRequest {
  pub component_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteComponentActionRequest {
  pub action_token: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ComponentActionImpact {
  pub ssh_connections: Option<u32>,
  pub port_forwards: Option<u32>,
  pub vpn_connections: Option<u32>,
  pub terminal_sessions: Option<u32>,
  pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentActionPreflight {
  pub action_token: String,
  pub component_id: String,
  pub component: &'static str,
  pub location: &'static str,
  pub host_id: Option<String>,
  pub label: String,
  pub action: ComponentAction,
  pub running: Option<ComponentVersionInfo>,
  pub available: Option<ComponentVersionInfo>,
  pub impact: ComponentActionImpact,
}

#[derive(Debug, Serialize)]
pub struct ComponentActionResult {
  pub component_id: String,
  pub component: &'static str,
  pub location: &'static str,
  pub host_id: Option<String>,
  pub action: ComponentAction,
  pub running: Option<ComponentVersionInfo>,
  pub detail: Option<String>,
}

fn compare(
  running: Option<&ComponentVersionInfo>,
  available: Option<&ComponentVersionInfo>,
) -> VersionStatus {
  let (Some(running), Some(available)) = (running, available) else {
    return VersionStatus::Unknown;
  };
  let versions = running
    .version
    .as_ref()
    .zip(available.version.as_ref())
    .and_then(|(actual, expected)| {
      Some((
        semver::Version::parse(actual).ok()?,
        semver::Version::parse(expected).ok()?,
      ))
    });
  if let Some((actual, expected)) = versions {
    match actual.cmp(&expected) {
      std::cmp::Ordering::Less => return VersionStatus::Outdated,
      std::cmp::Ordering::Greater => return VersionStatus::Newer,
      std::cmp::Ordering::Equal => {}
    }
  } else {
    return VersionStatus::Unknown;
  }
  if let Some((actual, expected)) = running
    .source_fingerprint
    .as_ref()
    .zip(available.source_fingerprint.as_ref())
  {
    return if actual == expected {
      VersionStatus::Current
    } else {
      VersionStatus::DifferentBuild
    };
  }
  if let Some((actual, expected)) = running
    .source_revision
    .as_ref()
    .zip(available.source_revision.as_ref())
  {
    if actual != expected {
      return VersionStatus::DifferentBuild;
    }
    if running.dirty == Some(false) && available.dirty == Some(false) {
      return VersionStatus::Current;
    }
  }
  VersionStatus::Unknown
}

#[cfg(test)]
mod tests {
  use super::*;

  fn info(version: &str, fingerprint: Option<&str>) -> ComponentVersionInfo {
    ComponentVersionInfo {
      version: Some(version.into()),
      source_fingerprint: fingerprint.map(str::to_owned),
      ..ComponentVersionInfo::default()
    }
  }

  #[test]
  fn comparison_distinguishes_builds_versions_and_missing_evidence() {
    let expected = info("0.2.0", Some("new"));
    for (actual, status) in [
      (info("0.1.0", Some("old")), VersionStatus::Outdated),
      (info("0.3.0", Some("next")), VersionStatus::Newer),
      (info("0.2.0", Some("old")), VersionStatus::DifferentBuild),
      (info("0.2.0", Some("new")), VersionStatus::Current),
      (info("0.2.0", None), VersionStatus::Unknown),
      (info("invalid", Some("new")), VersionStatus::Unknown),
    ] {
      assert_eq!(compare(Some(&actual), Some(&expected)), status);
    }
    assert_eq!(compare(None, Some(&expected)), VersionStatus::Unknown);
  }

  #[test]
  fn incompatible_protocol_takes_precedence_over_matching_build() {
    let mut expected = info("0.2.0", Some("same"));
    expected
      .protocols
      .push(ProtocolVersion::new("rmux", rmux_proto::PROTOCOL_VERSION));
    let mut actual = expected.clone();
    actual.protocols[0].version -= 1;
    let mut row = ComponentVersionRow::local("rmuxd", "rmuxd");
    row.running = Some(actual);
    row.available = Some(expected);
    row.compare();
    assert_eq!(row.status, VersionStatus::Incompatible);
  }

  #[test]
  fn incompatible_replacement_does_not_mislabel_the_running_protocol() {
    let mut running = expected_component_version();
    running
      .protocols
      .push(ProtocolVersion::new("rmux", rmux_proto::PROTOCOL_VERSION));
    let mut available = info("0.2.0", Some("next"));
    available.protocols.push(ProtocolVersion::new(
      "rmux",
      rmux_proto::PROTOCOL_VERSION + 1,
    ));
    let mut row = ComponentVersionRow::local("rmuxd", "rmuxd");
    row.running = Some(running);
    row.available = Some(available);
    row.compare();
    assert_eq!(row.status, VersionStatus::Current);
  }

  #[test]
  fn matching_stale_running_and_available_builds_are_not_current_with_this_app() {
    for component in ["ctld", "rmuxd", "taskd"] {
      let mut row = ComponentVersionRow::local(component, component);
      let mut stale = expected_component_version();
      stale.source_fingerprint = Some("another-component-build".into());
      stale.protocols = row.required_protocols.clone();
      row.running = Some(stale.clone());
      row.available = Some(stale);
      row.compare();
      row.note_available_mismatch();
      assert_eq!(row.status, VersionStatus::DifferentBuild);
      assert!(
        row
          .detail
          .as_deref()
          .unwrap()
          .contains("Restarting this helper will not align it with the app")
      );
      assert_eq!(row.running, row.available);
    }
  }

  #[test]
  fn required_protocol_is_preserved_when_running_and_available_helpers_are_both_old() {
    let mut row = ComponentVersionRow::local("ctld", "ctld");
    let mut old = info("0.1.0", Some("same-old-build"));
    old
      .protocols
      .push(ProtocolVersion::new("ctld", ctld_ipc::PROTOCOL_VERSION - 1));
    row.running = Some(old.clone());
    row.available = Some(old);
    row.compare();
    assert_eq!(row.status, VersionStatus::Incompatible);
    let value = serde_json::to_value(&row).unwrap();
    assert_eq!(
      value["required_protocols"][0]["version"],
      ctld_ipc::PROTOCOL_VERSION
    );
    assert_eq!(
      value["available"]["protocols"][0]["version"],
      ctld_ipc::PROTOCOL_VERSION - 1
    );
  }

  #[test]
  fn equal_revision_without_known_clean_sources_is_not_current() {
    let mut expected = info("0.2.0", None);
    expected.source_revision = Some("revision".into());
    let mut actual = expected.clone();
    expected.dirty = Some(false);
    assert_eq!(
      compare(Some(&actual), Some(&expected)),
      VersionStatus::Unknown
    );
    actual.dirty = Some(false);
    assert_eq!(
      compare(Some(&actual), Some(&expected)),
      VersionStatus::Current
    );
  }

  #[test]
  fn legacy_explanation_preserves_observed_protocols_without_inventing_a_build() {
    let mut row = ComponentVersionRow::local("taskd", "taskd");
    row.running = Some(ComponentVersionInfo {
      protocols: vec![ProtocolVersion::new("task", task_proto::PROTOCOL_VERSION)],
      ..ComponentVersionInfo::default()
    });
    row.compare();
    row.note_unreported_build();
    assert_eq!(row.status, VersionStatus::Unknown);
    assert!(
      row
        .detail
        .as_deref()
        .unwrap()
        .contains("does not report enough build information")
    );
    assert_eq!(row.running.as_ref().unwrap().protocols.len(), 1);
    assert!(row.running.unwrap().source_fingerprint.is_none());
  }

  #[test]
  fn maintenance_requests_cannot_supply_commands_or_endpoints() {
    assert!(
      serde_json::from_value::<PreflightRestartRequest>(serde_json::json!({
        "component_id": "rmuxd", "socket": "/tmp/other.sock",
      }))
      .is_err()
    );
    assert!(
      serde_json::from_value::<ExecuteComponentActionRequest>(serde_json::json!({
        "action_token": "opaque", "command": "restart",
      }))
      .is_err()
    );
  }
}
