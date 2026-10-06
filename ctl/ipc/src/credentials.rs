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

/// Credential scope for the destination and its authentication route.
///
/// VPN daemon sockets select a local transport owner, not a different password.
/// VPN profiles and verified remote account pins still distinguish routes. The
/// serialized identity of routes without VPNs is unchanged.
///
/// # Panics
/// Panics if a validated SSH target cannot be serialized.
#[must_use]
pub fn scope_id(target: &crate::SshTarget) -> String {
  let gateways: Vec<_> = target.gateways.iter().map(CredentialGateway).collect();
  digest_scope(&target.destination, &gateways)
}

/// Exact historical scope, including each VPN's local daemon socket.
///
/// Use only with the target's actual route to find an existing credential; do
/// not construct guessed socket paths or search unrelated hosts for passwords.
///
/// # Panics
/// Panics if a validated SSH target cannot be serialized.
#[must_use]
pub fn legacy_scope_id(target: &crate::SshTarget) -> String {
  digest_scope(&target.destination, &target.gateways)
}

/// Returns the current scope followed by the exact legacy scope, without duplicates.
///
/// # Panics
/// Panics if a validated SSH target cannot be serialized.
#[must_use]
pub fn lookup_scope_ids(target: &crate::SshTarget) -> Vec<String> {
  let current = scope_id(target);
  let legacy = legacy_scope_id(target);
  if current == legacy {
    vec![current]
  } else {
    vec![current, legacy]
  }
}

fn digest_scope(destination: &str, gateways: &impl Serialize) -> String {
  let bytes = serde_json::to_vec(&(destination, gateways))
    .expect("SSH credential scopes are always serializable");
  Sha256::digest(bytes)
    .iter()
    .fold(String::with_capacity(64), |mut encoded, byte| {
      write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
      encoded
    })
}

/// Keep the wire gateway intact and project only its credential identity here.
struct CredentialGateway<'a>(&'a crate::SshGateway);

impl Serialize for CredentialGateway<'_> {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct as _;

    // Destructure every field so future route additions require an explicit
    // decision about whether they distinguish saved credentials.
    let crate::SshGateway {
      kind,
      vpn,
      destination,
      hostname,
      user,
      port,
      identity_file,
      mode,
    } = self.0;
    let include_kind = *kind != crate::GatewayKind::Ssh;
    let mut route = serializer.serialize_struct(
      "SshGateway",
      6 + usize::from(include_kind) + usize::from(vpn.is_some()),
    )?;
    if include_kind {
      route.serialize_field("kind", kind)?;
    }
    if let Some(vpn) = vpn {
      let crate::VpnGateway {
        connection_id,
        socket_path: _,
        expected_remote_id,
      } = vpn;
      route.serialize_field(
        "vpn",
        &CredentialVpn {
          connection_id,
          expected_remote_id: expected_remote_id.as_deref(),
        },
      )?;
    }
    route.serialize_field("destination", destination)?;
    route.serialize_field("hostname", hostname)?;
    route.serialize_field("user", user)?;
    route.serialize_field("port", port)?;
    route.serialize_field("identity_file", identity_file)?;
    route.serialize_field("mode", mode)?;
    route.end()
  }
}

#[derive(Serialize)]
struct CredentialVpn<'a> {
  connection_id: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  expected_remote_id: Option<&'a str>,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{GatewayKind, SshGateway, SshGatewayMode, SshTarget, VpnGateway};

  fn target(gateways: Vec<SshGateway>) -> SshTarget {
    SshTarget {
      destination: "work".into(),
      ssh_config_alias: None,
      use_ssh_config_master: None,
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      gateways,
    }
  }

  fn ssh_gateway() -> SshGateway {
    SshGateway {
      kind: GatewayKind::Ssh,
      vpn: None,
      destination: "bastion".into(),
      hostname: Some("jump.example".into()),
      user: Some("alice".into()),
      port: Some(2200),
      identity_file: Some("/keys/jump".into()),
      mode: SshGatewayMode::NativeOnly,
    }
  }

  fn vpn_gateway(socket: &str) -> SshGateway {
    SshGateway {
      kind: GatewayKind::Vpn,
      vpn: Some(VpnGateway {
        connection_id: "office-vpn".into(),
        socket_path: socket.into(),
        expected_remote_id: None,
      }),
      destination: "office-vpn".into(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    }
  }

  fn socks_gateway() -> SshGateway {
    SshGateway {
      kind: GatewayKind::Socks5,
      vpn: None,
      destination: "127.0.0.1:1080".into(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    }
  }

  #[test]
  fn non_vpn_scopes_preserve_existing_password_identities() {
    for (target, expected) in [
      (
        target(Vec::new()),
        "6b87b49ec002a4307d29f28420190243ee7225e06747f43488a5295f74849b24",
      ),
      (
        target(vec![ssh_gateway(), socks_gateway()]),
        "751022a9c75fd5247f571e4e1d2696c6c718f475dabb5f5b8a220e249c160910",
      ),
    ] {
      assert_eq!(scope_id(&target), expected);
      assert_eq!(legacy_scope_id(&target), expected);
      assert_eq!(lookup_scope_ids(&target), vec![expected]);
    }
  }

  #[test]
  fn local_vpn_sockets_do_not_split_passwords_between_clients() {
    let desktop = target(vec![vpn_gateway("/tmp/desktop/ctld.sock")]);
    let cli = target(vec![vpn_gateway("/tmp/ctld-501/ctld-v1.sock")]);
    assert_eq!(scope_id(&desktop), scope_id(&cli));
    assert_ne!(legacy_scope_id(&desktop), legacy_scope_id(&cli));
    assert_eq!(
      lookup_scope_ids(&desktop),
      vec![scope_id(&desktop), legacy_scope_id(&desktop)],
    );
    assert_eq!(
      lookup_scope_ids(&cli),
      vec![scope_id(&cli), legacy_scope_id(&cli)],
    );
  }

  #[test]
  fn remote_vpn_sockets_are_ignored_but_account_pins_remain_distinct() {
    let mut first = target(vec![ssh_gateway(), vpn_gateway("/tmp/desktop.sock")]);
    first.gateways[1].vpn.as_mut().unwrap().expected_remote_id =
      Some("01234567-89ab-4def-8123-456789abcdef".into());
    let mut same_account = first.clone();
    same_account.gateways[1].vpn.as_mut().unwrap().socket_path = "/tmp/cli.sock".into();
    assert_eq!(scope_id(&first), scope_id(&same_account));
    assert_ne!(legacy_scope_id(&first), legacy_scope_id(&same_account));

    let mut other_account = same_account.clone();
    other_account.gateways[1]
      .vpn
      .as_mut()
      .unwrap()
      .expected_remote_id = Some("fedcba98-7654-4321-8fed-cba987654321".into());
    assert_ne!(scope_id(&first), scope_id(&other_account));
    other_account.gateways[1]
      .vpn
      .as_mut()
      .unwrap()
      .expected_remote_id = None;
    assert_ne!(scope_id(&first), scope_id(&other_account));
  }

  #[test]
  fn vpn_profiles_route_order_and_ssh_authentication_stay_distinct() {
    let original = target(vec![ssh_gateway(), vpn_gateway("/tmp/ctld.sock")]);
    let expected = scope_id(&original);
    let mut other_profile = original.clone();
    other_profile.gateways[1].destination = "other-vpn".into();
    other_profile.gateways[1]
      .vpn
      .as_mut()
      .unwrap()
      .connection_id = "other-vpn".into();
    assert_ne!(scope_id(&other_profile), expected);
    assert_ne!(
      scope_id(&target(vec![vpn_gateway("/tmp/ctld.sock"), ssh_gateway()])),
      expected,
    );

    for field in 0..6 {
      let mut changed = original.clone();
      let gateway = &mut changed.gateways[0];
      match field {
        0 => gateway.destination = "other-bastion".into(),
        1 => gateway.hostname = Some("other.example".into()),
        2 => gateway.user = Some("bob".into()),
        3 => gateway.port = Some(2201),
        4 => gateway.identity_file = Some("/keys/other".into()),
        _ => gateway.mode = SshGatewayMode::AgentRelayOnly,
      }
      assert_ne!(scope_id(&changed), expected, "gateway field {field}");
    }
    let socks = target(vec![vpn_gateway("/tmp/ctld.sock"), socks_gateway()]);
    let mut other_socks = socks.clone();
    other_socks.gateways[1].destination = "127.0.0.1:1081".into();
    assert_ne!(scope_id(&socks), scope_id(&other_socks));
  }

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
