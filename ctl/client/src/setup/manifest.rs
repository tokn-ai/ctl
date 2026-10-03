use super::Error;
use ctl_core::component::ProtocolInfo;
use serde::{Deserialize, Serialize};

pub(super) const BUNDLE_IDENTIFIER: &str = "dev.tokn-ai.ctl.ctld";
pub(super) const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
pub(super) const MAX_MANIFEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
  pub schema_version: u16,
  pub component: String,
  pub app_version: String,
  pub bundle_id: String,
  pub git_revision: String,
  pub target: String,
  pub bundle_identifier: String,
  pub team_identifier: String,
  pub signing_mode: String,
  pub notarized: bool,
  pub archive: String,
  pub sha256: String,
  pub archive_size: u64,
  pub protocols: Vec<ProtocolInfo>,
  #[serde(
    default,
    skip_serializing_if = "Option::is_none",
    deserialize_with = "development_identity"
  )]
  pub development: Option<Development>,
}

fn development_identity<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<Development>, D::Error> {
  Development::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Development {
  pub source_fingerprint: String,
  pub dirty: bool,
}

impl Manifest {
  pub fn parse(bytes: &[u8], version: &str, target: &str) -> Result<Self, Error> {
    Self::parse_with_policy(bytes, version, target, false)
  }

  pub fn parse_development(bytes: &[u8], version: &str, target: &str) -> Result<Self, Error> {
    Self::parse_with_policy(bytes, version, target, true)
  }

  /// Installed helpers are validated against their own release identity, not
  /// the discovering client's version. API compatibility is checked separately.
  pub fn parse_installed(bytes: &[u8], target: &str) -> Result<Self, Error> {
    let manifest = Self::decode(bytes)?;
    let version = manifest.app_version.clone();
    let development = manifest.development.is_some();
    Self::validate(manifest, &version, target, development)
  }

  fn parse_with_policy(
    bytes: &[u8],
    version: &str,
    target: &str,
    development: bool,
  ) -> Result<Self, Error> {
    Self::validate(Self::decode(bytes)?, version, target, development)
  }

  fn decode(bytes: &[u8]) -> Result<Self, Error> {
    if bytes.len() > MAX_MANIFEST_BYTES {
      return Err(Error::InvalidRelease("oversized manifest".into()));
    }
    serde_json::from_slice(bytes).map_err(|error| Error::InvalidRelease(error.to_string()))
  }

  fn validate(
    manifest: Self,
    version: &str,
    target: &str,
    development: bool,
  ) -> Result<Self, Error> {
    let valid_version = !version.is_empty()
      && version.len() <= 128
      && version
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    if !valid_version
      || !matches!(target, "aarch64-apple-darwin" | "x86_64-apple-darwin")
      || manifest.schema_version != 1
      || manifest.component != "ctld"
      || manifest.app_version != version
      || manifest.target != target
      || manifest.bundle_identifier != BUNDLE_IDENTIFIER
      || manifest.team_identifier.len() != 10
      || !manifest
        .team_identifier
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
      || manifest.git_revision.len() != 40
      || !manifest
        .git_revision
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
      || manifest.sha256.len() != 64
      || !manifest
        .sha256
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
      || manifest.archive != format!("ctld-{version}-{target}.app.tar.gz")
      || manifest.archive_size == 0
      || manifest.archive_size > MAX_ARCHIVE_BYTES
      || !valid_protocols(&manifest.protocols)
      || ["ctld", "ctld_lifecycle", "ctld_helper"]
        .iter()
        .any(|name| !manifest.protocols.iter().any(|entry| entry.name == *name))
    {
      return Err(Error::InvalidRelease(
        "manifest identity, signature policy, or archive does not match this release".into(),
      ));
    }
    let policy_valid = if development {
      manifest.signing_mode == "development"
        && !manifest.notarized
        && manifest.bundle_id == format!("dev.{}", manifest.sha256)
        && manifest.development.as_ref().is_some_and(|identity| {
          identity.source_fingerprint.len() == 64
            && identity
              .source_fingerprint
              .bytes()
              .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
    } else {
      manifest.signing_mode == "signed"
        && manifest.notarized
        && manifest.bundle_id == version
        && manifest.development.is_none()
    };
    if !policy_valid {
      return Err(Error::InvalidRelease(
        "manifest does not satisfy the selected release or development policy".into(),
      ));
    }
    Ok(manifest)
  }

  pub fn matches_protocols(&self, actual: &[ProtocolInfo]) -> bool {
    valid_protocols(&self.protocols)
      && ctl_core::component::protocols_match(&self.protocols, actual)
  }

  pub fn directory_name(&self) -> String {
    format!("{}-{}", self.app_version, self.target)
  }
}

fn valid_protocols(protocols: &[ProtocolInfo]) -> bool {
  !protocols.is_empty() && ctl_core::component::protocols_are_valid(protocols)
}

#[cfg(test)]
pub(super) fn fixture() -> Manifest {
  Manifest {
    schema_version: 1,
    component: "ctld".into(),
    app_version: "0.1.0".into(),
    bundle_id: "0.1.0".into(),
    git_revision: "a".repeat(40),
    target: "aarch64-apple-darwin".into(),
    bundle_identifier: BUNDLE_IDENTIFIER.into(),
    team_identifier: "ABCDEFGHIJ".into(),
    signing_mode: "signed".into(),
    notarized: true,
    archive: "ctld-0.1.0-aarch64-apple-darwin.app.tar.gz".into(),
    sha256: "b".repeat(64),
    archive_size: 1024,
    protocols: ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols,
    development: None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn installed_identity_is_independent_of_the_client_release() {
    let mut manifest = fixture();
    manifest.app_version = "0.2.0".into();
    manifest.bundle_id = manifest.app_version.clone();
    manifest.archive = format!(
      "ctld-{}-{}.app.tar.gz",
      manifest.app_version, manifest.target
    );
    let bytes = serde_json::to_vec(&manifest).unwrap();
    assert_eq!(
      Manifest::parse_installed(&bytes, &manifest.target).unwrap(),
      manifest
    );
    assert!(Manifest::parse(&bytes, "0.1.0", &manifest.target).is_err());
    assert!(Manifest::parse_installed(&bytes, "x86_64-apple-darwin").is_err());
    manifest.notarized = false;
    assert!(
      Manifest::parse_installed(&serde_json::to_vec(&manifest).unwrap(), &manifest.target).is_err()
    );
  }

  #[test]
  fn only_matching_signed_immutable_releases_are_accepted() {
    let original = fixture();
    let parse = |manifest: &Manifest| {
      Manifest::parse(
        &serde_json::to_vec(manifest).unwrap(),
        "0.1.0",
        "aarch64-apple-darwin",
      )
    };
    assert_eq!(parse(&original).unwrap(), original);
    for field in 0..10 {
      let mut invalid = original.clone();
      match field {
        0 => invalid.app_version = "0.2.0".into(),
        1 => invalid.bundle_id = "0.1.0-dev.deadbeef".into(),
        2 => invalid.signing_mode = "unsigned".into(),
        3 => invalid.notarized = false,
        4 => invalid.target = "x86_64-apple-darwin".into(),
        5 => invalid.archive = "../other.tar.gz".into(),
        6 => invalid.sha256 = "g".repeat(64),
        7 => invalid.team_identifier = "injected\"team".into(),
        8 => invalid.bundle_identifier = "dev.other.app".into(),
        _ => invalid.archive_size = MAX_ARCHIVE_BYTES + 1,
      }
      assert!(parse(&invalid).is_err(), "field {field}");
    }
    assert!(
      Manifest::parse(
        &vec![b' '; MAX_MANIFEST_BYTES + 1],
        "0.1.0",
        "aarch64-apple-darwin"
      )
      .is_err()
    );
  }

  #[test]
  fn manifests_require_a_complete_valid_advertised_contract_map() {
    let original = fixture();
    let parse = |manifest: &Manifest| {
      Manifest::parse_installed(&serde_json::to_vec(manifest).unwrap(), &manifest.target)
    };
    for index in 0..original.protocols.len() {
      let mut missing = original.clone();
      missing.protocols.remove(index);
      assert!(parse(&missing).is_err());
      let mut duplicate = original.clone();
      duplicate.protocols.push(duplicate.protocols[index].clone());
      assert!(parse(&duplicate).is_err());
      let mut invalid = original.clone();
      invalid.protocols[index].supported_versions.clear();
      assert!(parse(&invalid).is_err());
    }
    let mut missing = serde_json::to_value(&original).unwrap();
    missing.as_object_mut().unwrap().remove("protocols");
    assert!(
      Manifest::parse_installed(&serde_json::to_vec(&missing).unwrap(), &original.target).is_err()
    );
  }

  #[test]
  fn manifest_contract_map_must_match_all_executed_helper_advertisements() {
    let manifest = fixture();
    let mut actual = manifest.protocols.clone();
    actual.reverse();
    assert!(manifest.matches_protocols(&actual));
    actual[0].build += 1;
    assert!(!manifest.matches_protocols(&actual));
    actual = manifest.protocols.clone();
    let newer = ctl_core::protocol::ProtocolVersion::new(1, 1, 13);
    actual[0] = ProtocolInfo::new("ctld", 13, newer, &[ctl_ipc::PROTOCOL_VERSION, newer]);
    assert!(!manifest.matches_protocols(&actual));
    actual = manifest.protocols.clone();
    actual.pop();
    assert!(!manifest.matches_protocols(&actual));
  }

  #[test]
  fn advertised_contract_sets_match_without_requiring_array_order() {
    let mut manifest = fixture();
    let newer = ctl_core::protocol::ProtocolVersion::new(1, 1, 13);
    manifest.protocols[0] =
      ProtocolInfo::new("ctld", 13, newer, &[ctl_ipc::PROTOCOL_VERSION, newer]);
    let mut actual = manifest.protocols.clone();
    actual[0].supported_versions.reverse();
    actual.reverse();
    assert!(manifest.matches_protocols(&actual));

    let ctld = actual
      .iter_mut()
      .find(|entry| entry.name == "ctld")
      .unwrap();
    ctld.supported_versions = vec![newer];
    assert!(valid_protocols(&actual));
    assert!(!manifest.matches_protocols(&actual));

    actual = manifest.protocols.clone();
    actual[0].supported_versions[0] = ctl_core::protocol::ProtocolVersion::new(1, 0, 11);
    assert!(valid_protocols(&actual));
    assert!(!manifest.matches_protocols(&actual));
  }

  #[test]
  fn development_policy_is_explicit_and_binds_the_archive_and_source_identity() {
    let mut development = fixture();
    development.signing_mode = "development".into();
    development.notarized = false;
    development.bundle_id = format!("dev.{}", development.sha256);
    development.development = Some(Development {
      source_fingerprint: "c".repeat(64),
      dirty: true,
    });
    let parse = |manifest: &Manifest| {
      Manifest::parse_development(
        &serde_json::to_vec(manifest).unwrap(),
        "0.1.0",
        "aarch64-apple-darwin",
      )
    };
    assert_eq!(parse(&development).unwrap(), development);
    assert!(
      Manifest::parse(
        &serde_json::to_vec(&development).unwrap(),
        "0.1.0",
        "aarch64-apple-darwin"
      )
      .is_err()
    );
    assert!(parse(&fixture()).is_err());
    for field in 0..5 {
      let mut invalid = development.clone();
      match field {
        0 => invalid.signing_mode = "signed".into(),
        1 => invalid.notarized = true,
        2 => invalid.bundle_id = "dev.unrelated".into(),
        3 => invalid.development = None,
        _ => invalid.development.as_mut().unwrap().source_fingerprint = "G".repeat(64),
      }
      assert!(parse(&invalid).is_err(), "field {field}");
    }
    let mut release = serde_json::to_value(fixture()).unwrap();
    release["development"] = serde_json::Value::Null;
    assert!(
      Manifest::parse(
        &serde_json::to_vec(&release).unwrap(),
        "0.1.0",
        "aarch64-apple-darwin"
      )
      .is_err()
    );
  }
}
