//! Embedded identity shared by components built from the same Rust sources.

use serde::{Deserialize, Serialize};

#[cfg(feature = "executable")]
pub mod executable;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolInfo {
  pub name: String,
  pub version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ComponentInfo {
  pub build: ComponentBuildInfo,
  pub protocols: Vec<ProtocolInfo>,
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
}
