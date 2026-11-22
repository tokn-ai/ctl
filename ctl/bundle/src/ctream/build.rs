//! Portable build provenance in the optional ctream ZIP comment.

use serde::{Deserialize, Serialize};
use std::io;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const COMMENT_PREFIX: &str = "tokn-ctream-build-v1:";
const MAX_COMMENT_BYTES: usize = 1024;

/// Build provenance used to name an installed ctream bundle.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildMetadata {
  /// Version of the ctream preview package, when known.
  pub version: Option<String>,
  /// Full revision of the source used to build the preview, when known.
  pub revision: Option<String>,
  /// Build time in RFC 3339 format, when known.
  pub built_at: Option<String>,
  /// Source channel for naming this build.
  pub channel: String,
}

impl BuildMetadata {
  /// Reads provenance from a ctream archive comment, if present.
  /// # Errors
  /// Returns an error for invalid metadata with the recognized format prefix.
  pub fn from_archive_comment(comment: &[u8]) -> io::Result<Option<Self>> {
    let Some(json) = comment.strip_prefix(COMMENT_PREFIX.as_bytes()) else {
      return Ok(None);
    };
    if comment.len() > MAX_COMMENT_BYTES {
      return Err(invalid());
    }
    let build: Self = serde_json::from_slice(json).map_err(|_| invalid())?;
    build.validate()?;
    Ok(Some(build))
  }

  /// Encodes provenance as the optional comment for a ctream archive.
  /// # Errors
  /// Returns an error for invalid provenance.
  pub fn archive_comment(&self) -> io::Result<String> {
    self.validate()?;
    let json = serde_json::to_string(self).map_err(io::Error::other)?;
    let comment = format!("{COMMENT_PREFIX}{json}");
    if comment.len() > MAX_COMMENT_BYTES {
      return Err(invalid());
    }
    Ok(comment)
  }

  /// Validates build metadata supplied by the archive or workflow provider.
  /// # Errors
  /// Returns an error if any supplied field is invalid.
  pub fn validate(&self) -> io::Result<()> {
    if self
      .version
      .as_ref()
      .is_some_and(|version| version.len() > 72 || semver::Version::parse(version).is_err())
      || self.revision.as_ref().is_some_and(|revision| {
        !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
      })
      || self
        .built_at
        .as_ref()
        .is_some_and(|date| date.len() > 40 || OffsetDateTime::parse(date, &Rfc3339).is_err())
      || !valid_channel(&self.channel)
    {
      return Err(invalid());
    }
    Ok(())
  }
}

fn valid_channel(channel: &str) -> bool {
  matches!(
    channel,
    "stable" | "nightly" | "release" | "provided" | "local"
  ) || channel.strip_prefix("pr-").is_some_and(|number| {
    !number.starts_with('0')
      && number.bytes().all(|byte| byte.is_ascii_digit())
      && number.parse::<u64>().is_ok()
  })
}

fn invalid() -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, "invalid ctream build metadata")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recognizes_only_valid_ctream_build_comments() {
    let build = BuildMetadata {
      version: Some("0.1.0".into()),
      revision: Some("e".repeat(40)),
      built_at: Some("2026-11-22T06:15:30Z".into()),
      channel: "pr-198".into(),
    };
    let comment = build.archive_comment().unwrap();
    assert_eq!(
      BuildMetadata::from_archive_comment(comment.as_bytes()).unwrap(),
      Some(build)
    );
    assert_eq!(
      BuildMetadata::from_archive_comment(b"archive producer comment\xff").unwrap(),
      None
    );
    for invalid_json in [
      "{}",
      r#"{"channel":"../nightly"}"#,
      r#"{"version":"bogus","channel":"nightly"}"#,
      r#"{"built_at":"not a timestamp","channel":"nightly"}"#,
      r#"{"revision":"no-git-revision","channel":"nightly"}"#,
    ] {
      let invalid_comment = format!("{COMMENT_PREFIX}{invalid_json}");
      assert!(BuildMetadata::from_archive_comment(invalid_comment.as_bytes()).is_err());
    }
    let mut invalid_utf8 = COMMENT_PREFIX.as_bytes().to_vec();
    invalid_utf8.push(0xff);
    assert!(BuildMetadata::from_archive_comment(&invalid_utf8).is_err());
    let oversized = format!("{COMMENT_PREFIX}{}", " ".repeat(MAX_COMMENT_BYTES));
    assert!(BuildMetadata::from_archive_comment(oversized.as_bytes()).is_err());
  }
}
