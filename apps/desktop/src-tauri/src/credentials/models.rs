use serde::{Deserialize, Serialize};

use crate::dto::ConnectionTargetDto;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedTarget {
  pub name: String,
  pub target: ConnectionTargetDto,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
  pub targets: Vec<NamedTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetRequest {
  pub credential_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
  SshPassword,
  SshKeyPassphrase,
  SshCredential,
  VpnPassword,
  TailscaleSignIn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStorage {
  Keychain,
  VpnSettings,
  ContainerVolume,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialAction {
  Forget,
  ManageVpn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialRecord {
  pub credential_id: String,
  pub name: String,
  pub kind: CredentialKind,
  pub storage: CredentialStorage,
  pub target: Option<String>,
  pub account: Option<String>,
  pub created_at_ms: Option<i64>,
  pub updated_at_ms: Option<i64>,
  pub detail: Option<String>,
  pub action: CredentialAction,
  pub vpn_connection_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
  Keychain,
  Vpn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
  Ready,
  Partial,
  Unavailable,
  Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceStatus {
  pub source: CredentialSource,
  pub state: SourceState,
  pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialsSnapshot {
  pub credentials: Vec<CredentialRecord>,
  pub sources: Vec<SourceStatus>,
  pub metadata_import_required: bool,
  pub checked_at_ms: i64,
}
