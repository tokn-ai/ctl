use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::valid_workspace_text;
use crate::dto::ConnectionTargetDto;

/// A saved machine keeps its identity when its connection method changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHost {
  pub host_id: String,
  pub name: String,
  pub connection_methods: Vec<WorkspaceConnectionMethod>,
  pub preferred_method_id: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub remote_info: Option<ctl_proto::RemoteIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConnectionMethod {
  pub method_id: String,
  pub name: String,
  #[serde(deserialize_with = "deserialize_connection_settings")]
  pub target: ConnectionTargetDto,
}

// Transport DTOs tolerate additional runtime fields at the IPC boundary. Saved
// configuration must reject unknown settings instead of silently erasing them.
fn deserialize_connection_settings<'de, D>(deserializer: D) -> Result<ConnectionTargetDto, D::Error>
where
  D: serde::Deserializer<'de>,
{
  const FIELDS: &[&str] = &[
    "kind",
    "destination",
    "hostname",
    "user",
    "port",
    "identity_file",
    "gateway_route",
    "gateways",
    "remote_info",
  ];
  let value = serde_json::Value::deserialize(deserializer)?;
  if let Some(fields) = value.as_object()
    && let Some(field) = fields
      .keys()
      .find(|field| !FIELDS.contains(&field.as_str()))
  {
    return Err(serde::de::Error::custom(format!(
      "unsupported connection setting {field:?}"
    )));
  }
  serde_json::from_value(value).map_err(serde::de::Error::custom)
}

impl WorkspaceHost {
  pub(super) fn is_valid(&self, gateway_ids: &HashSet<&str>) -> bool {
    if !valid_workspace_text(&self.host_id) || !valid_workspace_text(&self.name) {
      return false;
    }
    if self.host_id == "local" {
      return self.name == "Local"
        && self.connection_methods.is_empty()
        && self.preferred_method_id.is_none()
        && self.remote_info.is_none();
    }
    if self.connection_methods.is_empty()
      || self.connection_methods.len() > 256
      || self
        .remote_info
        .as_ref()
        .is_some_and(|info| !info.is_valid())
    {
      return false;
    }
    let mut method_ids = HashSet::new();
    self
      .connection_methods
      .iter()
      .all(|method| method_ids.insert(method.method_id.as_str()) && method.is_valid(gateway_ids))
      && self
        .preferred_method_id
        .as_ref()
        .is_some_and(|id| method_ids.contains(id.as_str()))
  }
}

impl WorkspaceConnectionMethod {
  fn is_valid(&self, gateway_ids: &HashSet<&str>) -> bool {
    let ConnectionTargetDto::Ssh {
      destination,
      hostname,
      user,
      port,
      identity_file,
      remote_info,
      gateway_route,
      gateways,
    } = &self.target
    else {
      return false;
    };
    let mut route_ids = HashSet::new();
    valid_workspace_text(&self.method_id)
      && valid_workspace_text(&self.name)
      && valid_workspace_text(destination)
      && *port != Some(0)
      && [hostname, user, identity_file]
        .into_iter()
        .flatten()
        .all(|value| valid_workspace_text(value))
      // One account-owned environment is verified at the host level. Resolved
      // gateways are transport snapshots, never durable method configuration.
      && remote_info.is_none()
      && gateways.is_empty()
      && gateway_route.len() <= 8
      && gateway_route.iter().all(|step| {
        gateway_ids.contains(step.gateway_id.as_str())
          && route_ids.insert(step.gateway_id.as_str())
      })
  }
}
