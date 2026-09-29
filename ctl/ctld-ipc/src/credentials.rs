//! Metadata-only requests for the signed ctld credential helper.
//!
//! These messages travel over a one-shot process's stdin/stdout, independently
//! of the running daemon. They never contain credential contents.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

pub const MAX_REQUEST_BYTES: usize = 4 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
  SshPassword,
  SshKeyPassphrase,
  SshCredential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
  pub credential_id: String,
  pub scope_id: String,
  pub name: String,
  pub kind: CredentialKind,
  pub target: Option<String>,
  pub account: Option<String>,
  pub key_name: Option<String>,
  pub created_at_ms: Option<i64>,
  pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
  pub credentials: Vec<StoredCredential>,
  pub complete: bool,
  pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
  List,
  Forget { credential_id: String },
}

impl<'de> Deserialize<'de> for Request {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    // Serde's internally tagged unit variants otherwise ignore extra fields,
    // even with deny_unknown_fields. Use an empty struct variant on the wire.
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    enum Wire {
      List {},
      Forget { credential_id: String },
    }
    Ok(match Wire::deserialize(deserializer)? {
      Wire::List {} => Self::List,
      Wire::Forget { credential_id } => Self::Forget { credential_id },
    })
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
  Inventory { inventory: Inventory },
  Forgotten,
  Error { code: String, message: String },
}

/// Stable credential scope, preserving existing saved SSH credentials.
///
/// # Panics
/// Panics if a validated SSH target cannot be serialized.
#[must_use]
pub fn scope_id(target: &crate::SshTarget) -> String {
  // Only destination and gateway chain have historically defined this scope.
  let bytes = serde_json::to_vec(&(&target.destination, &target.gateways))
    .expect("SSH credential scopes are always serializable");
  Sha256::digest(bytes)
    .iter()
    .fold(String::with_capacity(64), |mut encoded, byte| {
      write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
      encoded
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn requests_are_bounded_to_operations_without_secret_fields() {
    assert_eq!(
      serde_json::to_string(&Request::List).unwrap(),
      r#"{"type":"list"}"#
    );
    assert!(serde_json::from_str::<Request>(r#"{"type":"list","password":"fixture"}"#).is_err());
    assert!(
      serde_json::from_str::<Request>(r#"{"type":"reveal","credential_id":"fixture"}"#).is_err()
    );
  }

  #[test]
  fn credential_response_contains_only_the_metadata_contract() {
    let value = serde_json::to_value(Response::Inventory {
      inventory: Inventory {
        credentials: vec![StoredCredential {
          credential_id: "item".into(),
          scope_id: "scope".into(),
          name: "Saved SSH credential".into(),
          kind: CredentialKind::SshCredential,
          target: None,
          account: None,
          key_name: None,
          created_at_ms: None,
          updated_at_ms: None,
        }],
        complete: true,
        warning: None,
      },
    })
    .unwrap();
    let credential = value["inventory"]["credentials"][0].as_object().unwrap();
    assert_eq!(credential.len(), 9);
    assert!(!credential.contains_key("password"));
    assert!(!credential.contains_key("passphrase"));
    assert!(!credential.contains_key("secret"));
    assert!(!credential.contains_key("value"));
  }
}
