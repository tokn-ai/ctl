use ctl_core::component::{ComponentBuildInfo, LegacyProtocolInfo, ProtocolInfo};
use ctl_core::protocol::ProtocolVersion as ContractVersion;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtocolVersion {
  pub name: String,
  pub build: u16,
  pub version: ContractVersion,
  pub supported_versions: Vec<ContractVersion>,
}

impl ProtocolVersion {
  #[cfg(test)]
  pub fn new(name: &str, version: ContractVersion) -> Self {
    Self {
      name: name.into(),
      build: version.build,
      version,
      supported_versions: vec![version],
    }
  }
}

impl From<ProtocolInfo> for ProtocolVersion {
  fn from(info: ProtocolInfo) -> Self {
    Self {
      name: info.name,
      build: info.build,
      version: info.version,
      supported_versions: info.supported_versions,
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
  pub fn from_component(info: ctl_core::component::ComponentInfo) -> Self {
    Self::from_build(
      info.build,
      info
        .protocols
        .into_iter()
        .map(ProtocolVersion::from)
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
  pub host_key: Option<String>,
  pub host_name: Option<String>,
  pub observation: &'static str,
  pub status: VersionStatus,
  pub running: Option<ComponentVersionInfo>,
  pub available: Option<ComponentVersionInfo>,
  pub installed: Option<ComponentVersionInfo>,
  pub restart_required: bool,
  pub legacy_protocols: Vec<LegacyProtocolInfo>,
  pub required_protocols: Vec<ProtocolVersion>,
  pub restart_supported: bool,
  pub action: Option<ComponentAction>,
  pub detail: Option<String>,
  pub error: Option<String>,
  pub error_code: Option<String>,
  pub connected: Option<bool>,
}

impl ComponentVersionRow {
  pub fn local(component: &'static str, label: &str) -> Self {
    Self {
      component_id: component.into(),
      component,
      label: label.into(),
      location: "local",
      host_id: None,
      host_key: None,
      host_name: None,
      observation: "running",
      status: VersionStatus::Unknown,
      running: None,
      available: None,
      installed: None,
      restart_required: false,
      legacy_protocols: Vec::new(),
      required_protocols: required_protocols(component),
      restart_supported: false,
      action: None,
      detail: None,
      error: None,
      error_code: None,
      connected: None,
    }
  }

  pub fn compare(&mut self) {
    self.compare_installed();
    self.status = compare(self.running.as_ref(), Some(&expected_component_version()));
    if self.running.as_ref().is_some_and(|running| {
      running.protocols.iter().any(|protocol| {
        self.required_protocols.iter().any(|expected| {
          expected.name == protocol.name
            && !expected
              .supported_versions
              .iter()
              .any(|version| protocol.supported_versions.contains(version))
        })
      })
    }) {
      self.status = VersionStatus::Incompatible;
    }
  }

  pub fn compare_installed(&mut self) {
    self.restart_required = (self.observation == "legacy" || !self.legacy_protocols.is_empty())
      && self.installed.is_some()
      || self.running.is_some()
        && matches!(
          compare(self.running.as_ref(), self.installed.as_ref()),
          VersionStatus::Outdated | VersionStatus::Newer | VersionStatus::DifferentBuild
        );
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
  ComponentVersionInfo::from_build(ctl_core::component::build_info(), Vec::new())
}

fn required_protocols(component: &str) -> Vec<ProtocolVersion> {
  let infos = match component {
    "ctld" => vec![
      ProtocolInfo::new(
        "ctld",
        ctl_ipc::PROTOCOL_BUILD,
        ctl_ipc::PROTOCOL_VERSION,
        ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS,
      ),
      ProtocolInfo::new(
        "ctld_lifecycle",
        ctl_ipc::lifecycle::PROTOCOL_BUILD,
        ctl_ipc::lifecycle::PROTOCOL_VERSION,
        ctl_ipc::lifecycle::SUPPORTED_PROTOCOL_VERSIONS,
      ),
    ],
    "ctmuxd" => vec![
      ctmux_proto::protocol_info(),
      ctmux_ipc::local_control_protocol_info(),
    ],
    "ctl-taskd" => vec![
      ctl_task_proto::protocol_info(),
      ctl_task_proto::control::protocol_info(),
    ],
    "ctl_agent" => ctl_proto::agent_protocols(),
    _ => Vec::new(),
  };
  infos.into_iter().map(ProtocolVersion::from).collect()
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
  fn agent_rows_advertise_app_requirements_before_inspection() {
    let row = ComponentVersionRow::local("ctl_agent", "ctl-agent");
    let expected: Vec<_> = ctl_proto::agent_protocols()
      .into_iter()
      .map(ProtocolVersion::from)
      .collect();
    assert_ne!(expected, []);
    assert_eq!(row.required_protocols, expected);
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
      .push(ProtocolVersion::new("ctmux", ctmux_proto::PROTOCOL_VERSION));
    let mut actual = expected.clone();
    actual.protocols[0].version = ctl_core::protocol::ProtocolVersion::new(2, 0, 1);
    actual.protocols[0].supported_versions = vec![actual.protocols[0].version];
    actual.protocols[0].build = 1;
    let mut row = ComponentVersionRow::local("ctmuxd", "ctmuxd");
    row.running = Some(actual);
    row.available = Some(expected);
    row.compare();
    assert_eq!(row.status, VersionStatus::Incompatible);
  }

  #[test]
  fn a_newer_minor_contract_is_compatible_when_it_retains_the_published_contract() {
    let mut row = ComponentVersionRow::local("ctmuxd", "ctmuxd");
    let mut running = expected_component_version();
    let latest = ContractVersion::new(1, 1, 15);
    running.protocols = vec![ProtocolVersion::from(ProtocolInfo::new(
      "ctmux",
      15,
      latest,
      &[ctmux_proto::PROTOCOL_VERSION, latest],
    ))];
    row.running = Some(running);
    row.compare();
    assert_eq!(row.status, VersionStatus::Current);
  }

  #[test]
  fn the_same_major_without_an_implemented_shared_contract_is_incompatible() {
    let mut row = ComponentVersionRow::local("ctmuxd", "ctmuxd");
    let mut running = expected_component_version();
    running.protocols = vec![ProtocolVersion::new(
      "ctmux",
      ContractVersion::new(1, 1, 15),
    )];
    row.running = Some(running);
    row.compare();
    assert_eq!(row.status, VersionStatus::Incompatible);
  }

  #[test]
  fn incompatible_replacement_does_not_mislabel_the_running_protocol() {
    let mut running = expected_component_version();
    running
      .protocols
      .push(ProtocolVersion::new("ctmux", ctmux_proto::PROTOCOL_VERSION));
    let mut available = info("0.2.0", Some("next"));
    available.protocols.push(ProtocolVersion::new(
      "ctmux",
      ctl_core::protocol::ProtocolVersion::new(1, 0, ctmux_proto::PROTOCOL_BUILD + 1),
    ));
    let mut row = ComponentVersionRow::local("ctmuxd", "ctmuxd");
    row.running = Some(running);
    row.available = Some(available);
    row.compare();
    assert_eq!(row.status, VersionStatus::Current);
  }

  #[test]
  fn matching_stale_running_and_available_builds_are_not_current_with_this_app() {
    for component in ["ctld", "ctmuxd", "ctl-taskd"] {
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
  fn required_protocol_is_preserved_when_both_helpers_lack_a_supported_contract() {
    let mut row = ComponentVersionRow::local("ctld", "ctld");
    let unsupported = ctl_core::protocol::ProtocolVersion::new(1, 0, 11);
    let mut old = info("0.1.0", Some("same-old-build"));
    old
      .protocols
      .push(ProtocolVersion::new("ctld", unsupported));
    row.running = Some(old.clone());
    row.available = Some(old);
    row.compare();
    assert_eq!(row.status, VersionStatus::Incompatible);
    let value = serde_json::to_value(&row).unwrap();
    assert_eq!(
      value["required_protocols"][0]["version"],
      serde_json::to_value(ctl_ipc::PROTOCOL_VERSION).unwrap()
    );
    assert_eq!(
      value["available"]["protocols"][0]["version"],
      serde_json::to_value(unsupported).unwrap()
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
    let mut row = ComponentVersionRow::local("ctl-taskd", "ctl-taskd");
    row.running = Some(ComponentVersionInfo {
      protocols: vec![ProtocolVersion::new(
        "task",
        ctl_task_proto::PROTOCOL_VERSION,
      )],
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
        "component_id": "ctmuxd", "socket": "/tmp/other.sock",
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
