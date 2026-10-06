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
  #[serde(default = "metadata_import_needed")]
  pub metadata_import_required: bool,
}

fn metadata_import_needed() -> bool {
  true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
  List,
  /// Rejectable by older helpers before their interactive inventory can run.
  ListMetadata,
  ImportMetadata,
  /// Explicitly remove all owned SSH secrets; never returns their values.
  Clear {},
  Forget {
    credential_id: String,
  },
}

impl<'de> Deserialize<'de> for Request {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    // Serde's internally tagged unit variants otherwise ignore extra fields,
    // even with deny_unknown_fields. Use an empty struct variant on the wire.
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    enum Wire {
      List {},
      ListMetadata {},
      ImportMetadata {},
      Clear {},
      Forget { credential_id: String },
    }
    Ok(match Wire::deserialize(deserializer)? {
      Wire::List {} => Self::List,
      Wire::ListMetadata {} => Self::ListMetadata,
      Wire::ImportMetadata {} => Self::ImportMetadata,
      Wire::Clear {} => Self::Clear {},
      Wire::Forget { credential_id } => Self::Forget { credential_id },
    })
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
  Inventory {
    inventory: Inventory,
  },
  Imported,
  Forgotten,
  Cleared {
    credential_count: usize,
    identity_count: usize,
  },
  Error {
    code: String,
    message: String,
  },
}

/// Counts returned only after every planned secret and its index were cleared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearCounts {
  pub credential_count: usize,
  pub identity_count: usize,
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
    assert_eq!(
      serde_json::from_str::<Request>(r#"{"type":"import_metadata"}"#).unwrap(),
      Request::ImportMetadata,
    );
    assert!(
      serde_json::from_str::<Request>(r#"{"type":"import_metadata","password":"fixture"}"#)
        .is_err()
    );
  }

  #[test]
  fn clear_is_explicit_metadata_only_and_rejected_by_older_helpers() {
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    enum PreviousRequest {
      List {},
      ListMetadata {},
      ImportMetadata {},
      Forget { credential_id: String },
    }
    let encoded = serde_json::to_string(&Request::Clear {}).unwrap();
    assert!(matches!(
      serde_json::from_str::<PreviousRequest>(
        r#"{"type":"forget","credential_id":"fixture"}"#
      )
      .unwrap(),
      PreviousRequest::Forget { credential_id } if credential_id == "fixture"
    ));
    assert_eq!(encoded, r#"{"type":"clear"}"#);
    assert_eq!(
      serde_json::from_str::<Request>(&encoded).unwrap(),
      Request::Clear {},
    );
    assert!(serde_json::from_str::<PreviousRequest>(&encoded).is_err());
    for extra in ["password", "identity_id", "credential_id", "include_vpn"] {
      let value = serde_json::json!({"type": "clear", extra: "fixture"});
      assert!(serde_json::from_value::<Request>(value).is_err());
    }
    assert_eq!(
      serde_json::to_value(Response::Cleared {
        credential_count: 2,
        identity_count: 3,
      })
      .unwrap(),
      serde_json::json!({"type":"cleared", "credential_count":2, "identity_count":3}),
    );
    assert_eq!(crate::HELPER_API_BUILD, 3);
    assert_eq!(crate::HELPER_API_VERSION, crate::HELPER_API_CONTRACT_V1_1_3,);
    for supported in [
      crate::HELPER_API_CONTRACT_V1_0_1,
      crate::HELPER_API_CONTRACT_V1_1_2,
      crate::HELPER_API_CONTRACT_V1_1_3,
    ] {
      assert!(crate::SUPPORTED_HELPER_API_VERSIONS.contains(&supported));
    }
  }

  #[test]
  fn older_inventory_does_not_claim_metadata_has_been_imported() {
    let inventory: Inventory =
      serde_json::from_str(r#"{"credentials":[],"complete":true,"warning":null}"#).unwrap();
    assert!(inventory.metadata_import_required);
  }

  #[test]
  fn noninteractive_inventory_is_rejected_by_the_old_request_contract() {
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    enum OldRequest {
      List {},
    }
    let request = serde_json::to_string(&Request::ListMetadata).unwrap();
    assert_eq!(request, r#"{"type":"list_metadata"}"#);
    assert!(serde_json::from_str::<OldRequest>(&request).is_err());
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
        metadata_import_required: false,
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
