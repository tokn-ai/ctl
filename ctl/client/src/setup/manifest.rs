use super::Error;
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
}

impl Manifest {
  pub fn parse(bytes: &[u8], version: &str, target: &str) -> Result<Self, Error> {
    if bytes.len() > MAX_MANIFEST_BYTES {
      return Err(Error::InvalidRelease("oversized manifest".into()));
    }
    let manifest: Self =
      serde_json::from_slice(bytes).map_err(|error| Error::InvalidRelease(error.to_string()))?;
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
      || manifest.bundle_id != version
      || manifest.target != target
      || manifest.bundle_identifier != BUNDLE_IDENTIFIER
      || manifest.team_identifier.len() != 10
      || !manifest
        .team_identifier
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
      || manifest.signing_mode != "signed"
      || !manifest.notarized
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
    {
      return Err(Error::InvalidRelease(
        "manifest identity, signature policy, or archive does not match this release".into(),
      ));
    }
    Ok(manifest)
  }

  pub fn directory_name(&self) -> String {
    format!("{}-{}", self.app_version, self.target)
  }
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
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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
}
