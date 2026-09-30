use super::valid_workspace_text;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// serde's skip_serializing_if callback must take a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_ssh_gateway_kind(kind: &ctld_ipc::GatewayKind) -> bool {
  *kind == ctld_ipc::GatewayKind::Ssh
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSshGateway {
  #[serde(default, skip_serializing_if = "is_ssh_gateway_kind")]
  pub kind: ctld_ipc::GatewayKind,
  pub gateway_id: String,
  pub name: String,
  pub destination: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub hostname: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub user: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub port: Option<u16>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub identity_file: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub remote_info: Option<ctl_proto::RemoteIdentity>,
}
#[must_use]
pub fn validated_gateway_ids(gateways: &[WorkspaceSshGateway]) -> Option<HashSet<&str>> {
  let mut gateway_ids = HashSet::new();
  let invalid = gateways.len() > 256
    || gateways.iter().any(|gateway| {
      !gateway_ids.insert(gateway.gateway_id.as_str())
        || !valid_workspace_text(&gateway.gateway_id)
        || !valid_workspace_text(&gateway.name)
        || !valid_workspace_text(&gateway.destination)
        || gateway
          .destination
          .chars()
          .any(|value| matches!(value, ',' | '@'))
        || gateway.port == Some(0)
        || gateway.kind == ctld_ipc::GatewayKind::Vpn
        || (gateway.kind == ctld_ipc::GatewayKind::Socks5
          && (gateway.port.is_none()
            || gateway.hostname.is_some()
            || gateway.identity_file.is_some()
            || gateway.remote_info.is_some()))
        || gateway
          .remote_info
          .as_ref()
          .is_some_and(|info| !info.is_valid())
        || [
          gateway.hostname.as_ref(),
          gateway.user.as_ref(),
          gateway.identity_file.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| !valid_workspace_text(value))
        || gateway.hostname.as_ref().is_some_and(|value| {
          value.chars().any(|value| matches!(value, ',' | '@'))
            || value.chars().any(char::is_whitespace)
        })
        || gateway.user.as_ref().is_some_and(|value| {
          value.chars().any(|value| matches!(value, ',' | '@'))
            || value.chars().any(char::is_whitespace)
        })
    });
  (!invalid).then_some(gateway_ids)
}
