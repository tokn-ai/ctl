use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};

use super::models::{ComponentAction, ComponentVersionInfo, ComponentVersionRow, ProtocolVersion};

/// Metadata belongs to the transport whose lifetime the attachment actor owns.
/// Persisted host identities are never treated as current process observations.
#[derive(Debug, Clone)]
pub(crate) struct RemoteObservation {
  pub identity: ctl_proto::RemoteIdentity,
  pub handshake: ctmux_client::HandshakeInfo,
  pub label: String,
  pub host_id: Option<String>,
}

pub(super) fn rows(observations: Vec<RemoteObservation>) -> Vec<ComponentVersionRow> {
  let mut rows = BTreeMap::new();
  for observation in observations {
    let agent_protocols = observation
      .identity
      .protocols
      .clone()
      .into_iter()
      .map(ProtocolVersion::from)
      .collect::<Vec<_>>();
    let mut agent_info = ComponentVersionInfo::observed(
      observation.identity.agent_version.clone(),
      observation.identity.build.clone(),
      agent_protocols.clone(),
    );
    if agent_info.source_revision.is_none() {
      agent_info.source_revision = observation
        .identity
        .bundle
        .as_ref()
        .map(|bundle| bundle.git_revision.clone());
    }
    insert(
      &mut rows,
      "ctl_agent",
      "ctl-agent",
      &observation,
      agent_info,
    );
    let ctmux_info = ComponentVersionInfo::observed(
      observation.handshake.server_version.clone(),
      observation.handshake.build.clone(),
      observation
        .handshake
        .protocols
        .clone()
        .into_iter()
        .map(ProtocolVersion::from)
        .collect(),
    );
    insert(&mut rows, "ctmuxd", "ctmuxd", &observation, ctmux_info);
  }
  rows.into_values().collect()
}

fn insert(
  rows: &mut BTreeMap<String, ComponentVersionRow>,
  component: &'static str,
  label: &str,
  observation: &RemoteObservation,
  running: ComponentVersionInfo,
) {
  let expected_protocols = super::models::required_protocols(component);
  // Preserve different simultaneously attached builds on one environment;
  // duplicate panes for the same build collapse to one diagnostic row.
  let mut hash = Sha256::new();
  hash.update(observation.identity.remote_id.as_bytes());
  hash.update([0]);
  hash.update(observation.host_id.as_deref().unwrap_or("").as_bytes());
  hash.update([0]);
  hash.update(serde_json::to_vec(&running).expect("version metadata serializes"));
  let id = format!("remote:{component}:{:x}", hash.finalize());
  rows.entry(id.clone()).or_insert_with(|| {
    let mut row = ComponentVersionRow {
      component_id: id,
      component,
      label: format!("{label} — {}", observation.label),
      location: "remote",
      host_id: observation.host_id.clone(),
      host_key: Some(match &observation.host_id {
        Some(host_id) => format!("saved:{host_id}"),
        None => format!("account:{}", observation.identity.remote_id),
      }),
      host_name: Some(observation.label.clone()),
      observation: "running",
      status: super::models::VersionStatus::Unknown,
      running: Some(running),
      required_protocols: expected_protocols.clone(),
      installed: None,
      restart_required: false,
      legacy_protocols: Vec::new(),
      // The remote table explicitly labels this reference as "This app build".
      // Confirmed actions inspect the installed remote replacement separately.
      available: Some(ComponentVersionInfo::from_build(
        ctl_core::component::build_info(),
        expected_protocols,
      )),
      restart_supported: component == "ctmuxd" && observation.identity.ctmux_restart_supported,
      action: match component {
        "ctl_agent" => Some(ComponentAction::Reconnect),
        "ctmuxd" if observation.identity.ctmux_restart_supported => Some(ComponentAction::Restart),
        _ => None,
      },
      detail: Some(
        "Observed on an active terminal connection. Compared with this app's component build."
          .into(),
      ),
      error: None,
      error_code: None,
      connected: Some(true),
    };
    row.compare();
    row
  });
}

#[cfg(test)]
mod tests {
  use super::super::models::VersionStatus;
  use super::*;
  use std::time::Duration;

  fn observation() -> RemoteObservation {
    let build = ctl_core::component::build_info();
    RemoteObservation {
      identity: ctl_proto::RemoteIdentity {
        protocols: ctl_proto::agent_protocols(),
        remote_id: "4db8b2dd-f953-458a-9124-97449c22a71f".into(),
        agent_version: build.version.clone(),
        build: Some(build.clone()),
        ctmux_restart_supported: true,
        bundle: None,
      },
      handshake: ctmux_client::HandshakeInfo {
        server_version: build.version.clone(),
        protocol_version: ctmux_proto::PROTOCOL_VERSION,
        protocols: vec![ctmux_proto::protocol_info()],
        build: Some(build),
        attachment_liveness: ctmux_client::AttachmentLiveness {
          heartbeat_interval: Duration::from_secs(1),
          peer_timeout: Duration::from_secs(3),
        },
      },
      label: "Test environment".into(),
      host_id: Some("test-host".into()),
    }
  }

  #[test]
  fn host_groups_use_saved_identity_or_authenticated_account_not_display_names() {
    let saved = observation();
    let mut alias = saved.clone();
    alias.host_id = Some("another-saved-host".into());
    let mut unsaved = saved.clone();
    unsaved.host_id = None;
    let mut another_account = unsaved.clone();
    another_account.identity.remote_id = "another-account".into();
    let result = rows(vec![saved, alias, unsaved, another_account]);
    assert_eq!(result.len(), 8);
    let keys: std::collections::BTreeSet<_> = result
      .iter()
      .map(|row| row.host_key.as_deref().unwrap())
      .collect();
    assert_eq!(keys.len(), 4);
    assert!(keys.contains("saved:test-host"));
    assert!(keys.contains("saved:another-saved-host"));
    assert!(keys.contains("account:another-account"));
    assert!(
      result
        .iter()
        .all(|row| row.host_name.as_deref() == Some("Test environment"))
    );
  }

  #[test]
  fn duplicate_panes_collapse_but_different_live_builds_remain_visible() {
    let first = observation();
    let mut changed = first.clone();
    changed.handshake.build.as_mut().unwrap().source_fingerprint = "previous-build".into();
    let result = rows(vec![first.clone(), first, changed]);
    assert_eq!(result.len(), 3);
    assert_eq!(
      result
        .iter()
        .filter(|row| row.component == "ctl_agent")
        .count(),
      1
    );
    assert_eq!(
      result
        .iter()
        .filter(|row| row.status == VersionStatus::DifferentBuild)
        .count(),
      1
    );
    assert!(
      result.iter().all(|row| row.location == "remote"
        && row.observation == "running"
        && row.action.is_some())
    );
  }

  #[test]
  fn active_remote_companion_protocols_use_the_same_app_requirements_as_inspected_rows() {
    let mut observation = observation();
    observation
      .handshake
      .protocols
      .push(ctmux_ipc::local_control_protocol_info());
    let mut result = rows(vec![observation]);
    let row = result
      .iter_mut()
      .find(|row| row.component == "ctmuxd")
      .unwrap();
    assert_eq!(row.status, VersionStatus::Current);
    let protocol = row
      .running
      .as_mut()
      .unwrap()
      .protocols
      .iter_mut()
      .find(|protocol| protocol.name == "ctmux_control")
      .unwrap();
    *protocol = ProtocolVersion::new(
      "ctmux_control",
      ctl_core::protocol::ProtocolVersion::new(2, 0, 1),
    );
    row.compare();
    assert_eq!(row.status, VersionStatus::Incompatible);
  }

  #[test]
  fn legacy_metadata_is_unknown_and_saved_hosts_cannot_supply_live_rows() {
    assert_eq!(rows(Vec::new()), Vec::<ComponentVersionRow>::new());
    let mut legacy = observation();
    legacy.identity.build = None;
    legacy.handshake.build = None;
    let result = rows(vec![legacy]);
    assert_eq!(result.len(), 2);
    assert!(
      result
        .iter()
        .all(|row| row.status == VersionStatus::Unknown)
    );
    for row in result {
      let reference = row
        .available
        .expect("the app build is known independently of remote metadata");
      assert_eq!(
        reference.source_fingerprint,
        Some(ctl_core::component::build_info().source_fingerprint)
      );
      assert_eq!(reference.protocols, row.required_protocols);
      assert!(row.running.unwrap().source_fingerprint.is_none());
    }
  }
}
