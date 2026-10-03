//! Embedded identity shared by components built from the same Rust sources.

use crate::protocol::{ProtocolVersion, negotiate_versions, valid_offer};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolInfo {
  pub name: String,
  pub build: u16,
  pub version: ProtocolVersion,
  pub supported_versions: Vec<ProtocolVersion>,
}

impl ProtocolInfo {
  #[must_use]
  pub fn new(
    name: impl Into<String>,
    build: u16,
    version: ProtocolVersion,
    supported: &[ProtocolVersion],
  ) -> Self {
    Self {
      name: name.into(),
      build,
      version,
      supported_versions: supported.to_vec(),
    }
  }

  #[must_use]
  pub fn is_valid(&self) -> bool {
    !self.name.is_empty()
      && self.name.len() <= 128
      && self
        .name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
      && valid_offer(self.build, self.version, &self.supported_versions)
  }

  #[must_use]
  pub fn supports(&self, selected: ProtocolVersion) -> bool {
    self.is_valid() && self.supported_versions.contains(&selected)
  }

  #[must_use]
  pub fn negotiate(&self, local: &[ProtocolVersion]) -> Option<ProtocolVersion> {
    if !self.is_valid() {
      return None;
    }
    negotiate_versions(&self.supported_versions, local)
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ComponentInfo {
  pub build: ComponentBuildInfo,
  pub protocols: Vec<ProtocolInfo>,
}

impl ComponentInfo {
  /// Validates bounded component metadata, including every advertised protocol.
  #[must_use]
  pub fn is_valid(&self) -> bool {
    self.build.is_valid() && protocols_are_valid(&self.protocols)
  }
}

/// Validates advertisements even when legacy component build metadata is absent.
#[must_use]
pub fn protocols_are_valid(protocols: &[ProtocolInfo]) -> bool {
  protocols.len() <= 128
    && protocols.iter().enumerate().all(|(index, protocol)| {
      protocol.is_valid()
        && !protocols[..index]
          .iter()
          .any(|earlier| earlier.name == protocol.name)
    })
}

/// Compares complete valid maps; protocol and supported-version order is immaterial.
#[must_use]
pub fn protocols_match(left: &[ProtocolInfo], right: &[ProtocolInfo]) -> bool {
  protocols_are_valid(left)
    && protocols_are_valid(right)
    && left.len() == right.len()
    && left.iter().all(|entry| {
      right.iter().any(|observed| {
        entry.name == observed.name
          && entry.build == observed.build
          && entry.version == observed.version
          && entry.supported_versions.len() == observed.supported_versions.len()
          && entry
            .supported_versions
            .iter()
            .all(|version| observed.supported_versions.contains(version))
      })
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ComponentBuildInfo {
  pub version: String,
  pub source_revision: Option<String>,
  pub source_fingerprint: String,
  pub dirty: bool,
}

impl ComponentBuildInfo {
  /// Validates bounded metadata received across a process boundary.
  #[must_use]
  pub fn is_valid(&self) -> bool {
    let hex = |value: &str| value.bytes().all(|byte| byte.is_ascii_hexdigit());
    !self.version.is_empty()
      && self.version.len() <= 256
      && !self.version.chars().any(char::is_control)
      && self.source_fingerprint.len() == 64
      && hex(&self.source_fingerprint)
      && self
        .source_revision
        .as_ref()
        .is_none_or(|revision| matches!(revision.len(), 40 | 64) && hex(revision))
  }
}

/// Returns compile-time metadata; never inspects a replacement executable.
#[must_use]
pub fn build_info() -> ComponentBuildInfo {
  ComponentBuildInfo {
    version: env!("CARGO_PKG_VERSION").into(),
    source_revision: match env!("COMPONENT_SOURCE_REVISION") {
      "" => None,
      revision => Some(revision.into()),
    },
    source_fingerprint: env!("COMPONENT_SOURCE_FINGERPRINT").into(),
    dirty: env!("COMPONENT_SOURCE_DIRTY") == "true",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const OLD: ProtocolVersion = ProtocolVersion::new(1, 0, 13);
  const NEW: ProtocolVersion = ProtocolVersion::new(1, 1, 15);

  #[test]
  fn identity_contains_a_deterministic_source_digest() {
    let info = super::build_info();
    assert_eq!(info.source_fingerprint.len(), 64);
    assert!(
      info
        .source_fingerprint
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
    );
    assert_eq!(info, super::build_info());
    assert!(info.is_valid());
  }

  #[test]
  fn malformed_metadata_cannot_claim_a_valid_component_identity() {
    let mut info = super::build_info();
    info.source_revision = None;
    assert!(info.is_valid());
    info.source_fingerprint = "invalid".into();
    assert!(!info.is_valid());
    info = super::build_info();
    info.version.push('\n');
    assert!(!info.is_valid());
    info = super::build_info();
    info.source_revision = Some("unverified-revision".into());
    assert!(!info.is_valid());
  }

  #[test]
  fn protocol_metadata_advertises_explicit_older_contracts() {
    let protocol = ProtocolInfo::new("test", 15, NEW, &[OLD, NEW]);
    assert!(protocol.is_valid());
    assert!(protocol.supports(OLD));
    assert!(protocol.supports(NEW));
    assert!(!protocol.supports(ProtocolVersion::new(1, 0, 14)));
    assert_eq!(protocol.negotiate(&[OLD]), Some(OLD));
    assert_eq!(
      serde_json::to_value(&protocol).unwrap()["version"],
      "1.1.15"
    );
  }

  #[test]
  fn invalid_or_duplicate_protocol_metadata_is_rejected() {
    let valid = ProtocolInfo::new("test", 15, NEW, &[OLD, NEW]);
    for name in [
      String::new(),
      "a".repeat(129),
      "test protocol".into(),
      "test\n".into(),
    ] {
      assert!(!ProtocolInfo::new(name, 15, NEW, &[OLD, NEW]).is_valid());
    }
    let mut info = ComponentInfo {
      build: build_info(),
      protocols: vec![valid.clone(), valid],
    };
    assert!(!info.is_valid());
    info.protocols.pop();
    assert!(info.is_valid());
    info.protocols[0].supported_versions.clear();
    assert!(!info.is_valid());
  }
}
