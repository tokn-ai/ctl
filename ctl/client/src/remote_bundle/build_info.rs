//! Optional publication metadata kept outside the component and payload identities.

use super::{BundleSet, Error, read_bounded_file};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

pub(crate) const FILE_NAME: &str = "build-info.json";
pub(crate) const MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BuildInfo {
  schema_version: u16,
  app_version: String,
  bundle_id: String,
  git_revision: String,
  pub built_at: String,
}

impl BuildInfo {
  pub(crate) fn new(manifest: &BundleSet, built_at: String) -> Result<Self, Error> {
    if !valid_timestamp(&built_at) {
      return Err(Error::Invalid("invalid build UTC timestamp".into()));
    }
    Ok(Self {
      schema_version: 1,
      app_version: manifest.app_version.clone(),
      bundle_id: manifest.bundle_id.clone(),
      git_revision: manifest.git_revision.clone(),
      built_at,
    })
  }

  pub(crate) fn parse(bytes: &[u8], manifest: &BundleSet) -> Result<Self, Error> {
    Self::parse_identity(
      bytes,
      &manifest.app_version,
      &manifest.bundle_id,
      &manifest.git_revision,
    )
  }

  pub(crate) fn parse_identity(
    bytes: &[u8],
    app_version: &str,
    bundle_id: &str,
    git_revision: &str,
  ) -> Result<Self, Error> {
    if bytes.len() > MAX_BYTES {
      return Err(Error::Invalid(
        "build metadata exceeds its size limit".into(),
      ));
    }
    let info: Self = serde_json::from_slice(bytes)
      .map_err(|_| Error::Invalid("invalid build metadata JSON".into()))?;
    if info.schema_version != 1
      || info.app_version != app_version
      || info.bundle_id != bundle_id
      || info.git_revision != git_revision
      || !valid_timestamp(&info.built_at)
    {
      return Err(Error::Invalid(
        "build metadata differs from its publication or has an invalid UTC timestamp".into(),
      ));
    }
    Ok(info)
  }
}

pub(crate) fn read(directory: &Path, manifest: &BundleSet) -> Result<Option<String>, Error> {
  match read_bounded_file(&directory.join(FILE_NAME), MAX_BYTES) {
    Ok(bytes) => Ok(Some(BuildInfo::parse(&bytes, manifest)?.built_at)),
    Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}

pub(crate) fn valid_timestamp(value: &str) -> bool {
  let bytes = value.as_bytes();
  if !(bytes.len() == 20 || (22..=30).contains(&bytes.len()))
    || bytes[4] != b'-'
    || bytes[7] != b'-'
    || bytes[10] != b'T'
    || bytes[13] != b':'
    || bytes[16] != b':'
    || bytes.last() != Some(&b'Z')
    || (bytes.len() > 20
      && (bytes[19] != b'.' || !bytes[20..bytes.len() - 1].iter().all(u8::is_ascii_digit)))
  {
    return false;
  }
  bytes[17..19] != *b"60"
    && time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).is_ok()
}
