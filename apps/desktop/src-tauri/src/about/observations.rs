use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};

use super::models::{ComponentAction, ComponentVersionInfo, ComponentVersionRow, ProtocolVersion};

/// Metadata belongs to the transport whose lifetime the attachment actor owns.
/// Persisted host identities are never treated as current process observations.
#[derive(Debug, Clone)]
pub(crate) struct RemoteObservation {
  pub identity: ctl_proto::RemoteIdentity,
  pub handshake: rmux_client::HandshakeInfo,
  pub label: String,
  pub host_id: Option<String>,
}

pub(super) fn rows(observations: Vec<RemoteObservation>) -> Vec<ComponentVersionRow> {
  let mut rows = BTreeMap::new();
  for observation in observations {
    let agent_protocols = vec![ProtocolVersion::new(
      "ctl_identity",
      ctl_proto::IDENTITY_PROTOCOL_VERSION,
    )];
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
      agent_protocols,
    );
    let rmux_info = ComponentVersionInfo::observed(
      observation.handshake.server_version.clone(),
      observation.handshake.build.clone(),
      vec![ProtocolVersion::new(
        "rmux",
        observation.handshake.protocol_version,
      )],
    );
    insert(
      &mut rows,
      "rmuxd",
      "rmuxd",
      &observation,
      rmux_info,
      vec![ProtocolVersion::new("rmux", rmux_proto::PROTOCOL_VERSION)],
    );
  }
  rows.into_values().collect()
}

fn insert(
  rows: &mut BTreeMap<String, ComponentVersionRow>,
  component: &'static str,
  label: &str,
  observation: &RemoteObservation,
  running: ComponentVersionInfo,
  expected_protocols: Vec<ProtocolVersion>,
) {
  // Preserve different simultaneously attached builds on one environment;
  // duplicate panes for the same build collapse to one diagnostic row.
  let mut hash = Sha256::new();
  hash.update(observation.identity.remote_id.as_bytes());
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
      observation: "running",
      status: super::models::VersionStatus::Unknown,
      running: Some(running),
      required_protocols: expected_protocols.clone(),
      // The remote table explicitly labels this reference as "This app build".
      // Confirmed actions inspect the installed remote replacement separately.
      available: Some(ComponentVersionInfo::from_build(
        component_info::build_info(),
        expected_protocols,
      )),
      restart_supported: component == "rmuxd" && observation.identity.rmux_restart_supported,
      action: match component {
        "ctl_agent" => Some(ComponentAction::Reconnect),
        "rmuxd" if observation.identity.rmux_restart_supported => Some(ComponentAction::Restart),
        _ => None,
      },
      detail: Some(
        "Observed on an active terminal connection. Compared with this app's component build."
          .into(),
      ),
      error: None,
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
    let build = component_info::build_info();
    RemoteObservation {
      identity: ctl_proto::RemoteIdentity {
        remote_id: "4db8b2dd-f953-458a-9124-97449c22a71f".into(),
        agent_version: build.version.clone(),
        build: Some(build.clone()),
        rmux_restart_supported: true,
        bundle: None,
      },
      handshake: rmux_client::HandshakeInfo {
        server_version: build.version.clone(),
        protocol_version: rmux_proto::PROTOCOL_VERSION,
        build: Some(build),
        attachment_liveness: rmux_client::AttachmentLiveness {
          heartbeat_interval: Duration::from_secs(1),
          peer_timeout: Duration::from_secs(3),
        },
      },
      label: "Test environment".into(),
      host_id: Some("test-host".into()),
    }
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
  fn legacy_metadata_is_unknown_and_saved_hosts_cannot_supply_live_rows() {
    assert!(rows(Vec::new()).is_empty());
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
        Some(component_info::build_info().source_fingerprint)
      );
      assert_eq!(reference.protocols, row.required_protocols);
      assert!(row.running.unwrap().source_fingerprint.is_none());
    }
  }
}
