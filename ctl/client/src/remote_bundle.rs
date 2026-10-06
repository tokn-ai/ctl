//! Trusted remote component bundles shared by desktop and standalone clients.

use ctl_core::component::{ComponentBuildInfo, ComponentInfo};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_BUNDLE_BYTES: usize = 128 * 1024 * 1024;
const MAX_BUNDLE_SET_BYTES: usize = 64 * 1024;
const BUNDLE_SET_FILE: &str = "bundle-set.json";
const RELEASE_ROOT: &str = "https://github.com/tokn-ai/ctl/releases/download";
const SUPPORTED_TARGETS: [&str; 4] = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
];

mod cache;
pub(crate) mod compatibility;
pub use cache::{BundleCacheEntry, read_compatible_cached_bundle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
  pub os: String,
  pub architecture: String,
}

impl Platform {
  /// Parses the fixed platform probe after its shell preface has been removed.
  ///
  /// # Errors
  /// Rejects missing, noisy, or malformed probe responses.
  pub fn parse_probe(output: &str) -> Result<Self, Error> {
    let mut lines = output.lines();
    let marker = lines.next().map(str::trim_end);
    let os = lines
      .next()
      .map(str::trim)
      .filter(|value| !value.is_empty());
    let architecture = lines
      .next()
      .map(str::trim)
      .filter(|value| !value.is_empty());
    let (Some("ctl-platform-v1"), Some(os), Some(architecture)) = (marker, os, architecture) else {
      return Err(Error::Invalid(
        "invalid remote platform probe response".into(),
      ));
    };
    if lines.any(|line| !line.trim().is_empty()) {
      return Err(Error::Invalid(
        "invalid remote platform probe response".into(),
      ));
    }
    Ok(Self {
      os: os.into(),
      architecture: architecture.into(),
    })
  }

  /// Maps the supported remote Unix platforms to portable bundle targets.
  ///
  /// # Errors
  /// Rejects platforms for which this project does not ship a remote bundle.
  pub fn target_triple(&self) -> Result<&'static str, Error> {
    match (self.os.as_str(), self.architecture.as_str()) {
      ("Linux", "x86_64" | "amd64") => Ok("x86_64-unknown-linux-musl"),
      ("Linux", "aarch64" | "arm64") => Ok("aarch64-unknown-linux-musl"),
      ("Darwin", "x86_64" | "amd64") => Ok("x86_64-apple-darwin"),
      ("Darwin", "aarch64" | "arm64") => Ok("aarch64-apple-darwin"),
      _ => Err(Error::UnsupportedTarget(format!(
        "{} {}",
        self.os, self.architecture
      ))),
    }
  }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSet {
  schema_version: u32,
  pub app_version: String,
  pub bundle_id: String,
  pub git_revision: String,
  targets: BTreeMap<String, BundleTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundleTarget {
  archive: String,
  sha256: String,
  #[serde(default, deserialize_with = "compatibility::deserialize_components")]
  pub(crate) components: Option<BTreeMap<String, ComponentInfo>>,
}

impl BundleSet {
  /// Validates a complete bundle set before any archive path or URL is used.
  ///
  /// # Errors
  /// Rejects oversized manifests, other versions, unsafe paths, missing targets,
  /// and malformed build identities or checksums.
  pub fn parse(bytes: &[u8], expected_version: &str) -> Result<Self, Error> {
    let manifest = Self::parse_intrinsic(bytes)?;
    if !safe_id(expected_version) || manifest.app_version != expected_version {
      return Err(Error::Invalid(format!(
        "remote bundle set targets version {}, not {expected_version}",
        manifest.app_version
      )));
    }
    Ok(manifest)
  }

  /// Validates the bundle's own immutable identity without selecting a client release.
  ///
  /// # Errors
  /// Rejects malformed, unbounded, unsafe, or inconsistent bundle metadata.
  pub fn parse_intrinsic(bytes: &[u8]) -> Result<Self, Error> {
    if bytes.len() > MAX_BUNDLE_SET_BYTES {
      return Err(Error::Invalid(
        "remote bundle-set manifest exceeds its size limit".into(),
      ));
    }
    let manifest: Self = serde_json::from_slice(bytes)
      .map_err(|_| Error::Invalid("invalid remote bundle-set JSON".into()))?;
    if !matches!(manifest.schema_version, 1 | 2) {
      return Err(Error::Invalid(
        "unsupported remote bundle-set schema version".into(),
      ));
    }
    if !safe_id(&manifest.app_version) {
      return Err(Error::Invalid(
        "invalid remote bundle product version".into(),
      ));
    }
    if !safe_id(&manifest.bundle_id) {
      return Err(Error::Invalid("invalid remote bundle ID".into()));
    }
    if manifest.git_revision.len() != 40
      || !manifest
        .git_revision
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
    {
      return Err(Error::Invalid("invalid remote bundle Git revision".into()));
    }
    let development_id = format!(
      "{}-dev.{}",
      manifest.app_version,
      &manifest.git_revision[..12]
    );
    if manifest.bundle_id != manifest.app_version && manifest.bundle_id != development_id {
      return Err(Error::Invalid(
        "remote bundle ID does not match its version and Git revision".into(),
      ));
    }
    if manifest.targets.len() != SUPPORTED_TARGETS.len()
      || SUPPORTED_TARGETS
        .iter()
        .any(|target| !manifest.targets.contains_key(*target))
    {
      return Err(Error::Invalid(
        "remote bundle set does not contain every supported target".into(),
      ));
    }
    for (target, bundle) in &manifest.targets {
      compatibility::validate_components(&manifest, bundle.components.as_ref())?;
      let expected_archive = format!("ctl-agent-bundle-{}-{target}.tar.gz", manifest.bundle_id);
      if bundle.archive != expected_archive
        || bundle.sha256.len() != 64
        || !bundle.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
      {
        return Err(Error::Invalid(format!(
          "invalid remote bundle metadata for {target}"
        )));
      }
    }
    Ok(manifest)
  }

  /// Requires the exact clean source build before replacing remote components.
  ///
  /// # Errors
  /// Rejects uncommitted, unidentified, or different source builds.
  pub fn verify_revision(&self, expected: &ComponentBuildInfo) -> Result<(), Error> {
    if !expected.is_valid()
      || expected.dirty
      || expected.version != self.app_version
      || expected.source_revision.as_deref() != Some(&self.git_revision)
    {
      return Err(Error::Stale(
        "remote component bundle does not match this clean client build; synchronize bundles for its exact source revision".into(),
      ));
    }
    Ok(())
  }

  /// Checks every client/service and internal companion edge by explicit contracts.
  /// Schema-1 bundles have no advertisements and cannot be reused across builds.
  ///
  /// # Errors
  /// Rejects unsupported targets or invalid schema-2 component advertisements.
  pub fn is_compatible(&self, target: &str) -> Result<bool, Error> {
    let entry = self.target(target)?;
    Ok(
      entry
        .components
        .as_ref()
        .is_some_and(compatibility::compatible),
    )
  }

  pub(crate) fn target(&self, target: &str) -> Result<&BundleTarget, Error> {
    self
      .targets
      .get(target)
      .ok_or_else(|| Error::UnsupportedTarget(target.into()))
  }

  /// Canonical archive filenames from this validated complete bundle set.
  pub fn archive_names(&self) -> impl Iterator<Item = &str> {
    self.targets.values().map(|target| target.archive.as_str())
  }

  /// Returns a validated archive filename for one supported target.
  ///
  /// # Errors
  /// Rejects targets outside this complete bundle set.
  pub fn archive_name(&self, target: &str) -> Result<&str, Error> {
    Ok(&self.target(target)?.archive)
  }

  pub(crate) fn verify_archive_bytes(&self, target: &str, archive: &[u8]) -> Result<(), Error> {
    let entry = self.target(target)?;
    if archive.len() > MAX_BUNDLE_BYTES {
      return Err(Error::Invalid(
        "remote component archive exceeds its size limit".into(),
      ));
    }
    let actual = format!("{:x}", Sha256::digest(archive));
    if !actual.eq_ignore_ascii_case(&entry.sha256) {
      return Err(Error::Invalid(
        "remote component archive failed checksum verification".into(),
      ));
    }
    Ok(())
  }

  fn verify_archive(
    &self,
    target: &str,
    archive: Vec<u8>,
    manifest: Vec<u8>,
  ) -> Result<VerifiedBundle, Error> {
    self.verify_archive_bytes(target, &archive)?;
    if self.schema_version == 2 {
      compatibility::verify_archive(self, target, &archive)?;
    }
    let entry = self.target(target)?;
    Ok(VerifiedBundle {
      app_version: self.app_version.clone(),
      bundle_id: self.bundle_id.clone(),
      git_revision: self.git_revision.clone(),
      archive,
      file_name: entry.archive.clone(),
      manifest,
    })
  }
}

#[derive(Debug)]
pub struct VerifiedBundle {
  pub app_version: String,
  pub bundle_id: String,
  pub git_revision: String,
  pub archive: Vec<u8>,
  pub file_name: String,
  /// The bounded, validated bundle-set document that authenticated this archive.
  pub manifest: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("remote component bundle is unavailable: {0}")]
  NotAvailable(String),
  #[error("invalid remote component bundle: {0}")]
  Invalid(String),
  #[error("stale remote component bundle: {0}")]
  Stale(String),
  #[error("no remote component bundle is available for {0}")]
  UnsupportedTarget(String),
  #[error("could not read remote component bundle: {0}")]
  Io(#[from] io::Error),
  #[error("could not download remote component bundle: {0}")]
  Download(String),
}

/// Reads the first locally available bundle set from trusted directories.
/// Missing directories return `None`; a present invalid set never silently
/// falls back to a different build. Callers should run this filesystem work on
/// a blocking worker when invoked from an asynchronous UI.
///
/// # Errors
/// Rejects invalid/stale manifests, excessive file sizes, and archive corruption.
pub fn read_verified_bundle(
  directories: &[PathBuf],
  target: &str,
  expected: &ComponentBuildInfo,
) -> Result<Option<VerifiedBundle>, Error> {
  validate_target(target)?;
  for directory in directories {
    let bytes = match read_bounded_file(&directory.join(BUNDLE_SET_FILE), MAX_BUNDLE_SET_BYTES) {
      Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => continue,
      result => result?,
    };
    let manifest = BundleSet::parse(&bytes, &expected.version)?;
    manifest.verify_revision(expected)?;
    let archive = read_bounded_file(
      &directory.join(&manifest.target(target)?.archive),
      MAX_BUNDLE_BYTES,
    )?;
    return manifest.verify_archive(target, archive, bytes).map(Some);
  }
  Ok(None)
}

/// Reads valid schema-2 bundles compatible with this client, independent of release.
/// Missing or valid incompatible candidates are skipped; malformed present inputs fail.
///
/// # Errors
/// Rejects unsafe, malformed, or checksum-invalid candidate bundles.
pub fn read_compatible_bundle(
  directories: &[PathBuf],
  target: &str,
) -> Result<Option<VerifiedBundle>, Error> {
  read_reusable(directories, target, None)
}

/// Reads compatible schema-2 bundles or an exact clean-client schema-1 bundle.
///
/// # Errors
/// Rejects unsafe, malformed, or checksum-invalid candidate bundles.
pub fn read_reusable_bundle(
  directories: &[PathBuf],
  target: &str,
  expected: &ComponentBuildInfo,
) -> Result<Option<VerifiedBundle>, Error> {
  read_reusable(directories, target, Some(expected))
}

fn read_reusable(
  directories: &[PathBuf],
  target: &str,
  expected: Option<&ComponentBuildInfo>,
) -> Result<Option<VerifiedBundle>, Error> {
  validate_target(target)?;
  for directory in directories {
    let bytes = match read_bounded_file(&directory.join(BUNDLE_SET_FILE), MAX_BUNDLE_SET_BYTES) {
      Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => continue,
      result => result?,
    };
    let manifest = BundleSet::parse_intrinsic(&bytes)?;
    let reusable = if manifest.schema_version == 1 {
      expected.is_some_and(|expected| manifest.verify_revision(expected).is_ok())
    } else {
      manifest.is_compatible(target)?
    };
    if reusable {
      let archive = read_bounded_file(
        &directory.join(manifest.archive_name(target)?),
        MAX_BUNDLE_BYTES,
      )?;
      return manifest.verify_archive(target, archive, bytes).map(Some);
    }
  }
  Ok(None)
}

/// Downloads this clean source build's published bundle from its fixed version
/// release, without selecting latest releases or executing external commands.
///
/// # Errors
/// Rejects unpublished releases, untrusted redirects, mismatched source builds,
/// oversized/truncated responses, and archive checksum failures.
pub async fn download_release_bundle(
  target: &str,
  expected: &ComponentBuildInfo,
) -> Result<VerifiedBundle, Error> {
  validate_target(target)?;
  if !safe_id(&expected.version) {
    return Err(Error::Invalid(
      "invalid client version for remote bundle download".into(),
    ));
  }
  let client = reqwest::Client::builder()
    .https_only(true)
    .connect_timeout(Duration::from_secs(15))
    .read_timeout(Duration::from_secs(30))
    .redirect(reqwest::redirect::Policy::custom(|attempt| {
      if attempt.previous().len() >= 5 || !trusted_url(attempt.url()) {
        attempt.error("untrusted remote bundle redirect")
      } else {
        attempt.follow()
      }
    }))
    .user_agent(concat!("ctl/", env!("CARGO_PKG_VERSION")))
    .build()
    .map_err(download_error)?;
  let root = format!("{RELEASE_ROOT}/v{}", expected.version);
  let bytes = download(
    &client,
    &format!("{root}/{BUNDLE_SET_FILE}"),
    MAX_BUNDLE_SET_BYTES,
  )
  .await?;
  let manifest = BundleSet::parse(&bytes, &expected.version)?;
  if !manifest.is_compatible(target)? {
    return Err(Error::NotAvailable(
      "published bundle does not advertise the required compatible component contracts".into(),
    ));
  }
  // A version release must not silently supply a main-branch development build.
  if manifest.bundle_id != manifest.app_version {
    return Err(Error::Invalid(
      "published release contains a development remote bundle".into(),
    ));
  }
  let archive = download(
    &client,
    &format!("{root}/{}", manifest.target(target)?.archive),
    MAX_BUNDLE_BYTES,
  )
  .await?;
  manifest.verify_archive(target, archive, bytes)
}

fn safe_id(value: &str) -> bool {
  !value.is_empty()
    && value.len() <= 128
    && value
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
}

fn validate_target(target: &str) -> Result<(), Error> {
  if SUPPORTED_TARGETS.contains(&target) {
    Ok(())
  } else {
    Err(Error::UnsupportedTarget(target.into()))
  }
}

fn read_bounded_file(path: &Path, maximum: usize) -> Result<Vec<u8>, Error> {
  #[cfg(unix)]
  let file = File::from(
    rustix::fs::open(
      path,
      rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC,
      rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)?,
  );
  #[cfg(not(unix))]
  let file = {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
      use std::os::windows::fs::OpenOptionsExt as _;
      options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    options.open(path)?
  };
  let metadata = file.metadata()?;
  #[cfg(windows)]
  {
    use std::os::windows::fs::MetadataExt as _;
    if metadata.file_attributes() & 0x0400 != 0 {
      return Err(Error::Invalid(
        "remote bundle files must not be reparse points".into(),
      ));
    }
  }
  if !metadata.is_file() {
    return Err(Error::Invalid(
      "remote bundle must contain regular files".into(),
    ));
  }
  if metadata.len() > maximum as u64 {
    return Err(Error::Invalid(
      "remote bundle file exceeds its size limit".into(),
    ));
  }
  let mut bytes = Vec::new();
  file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
  if bytes.len() > maximum {
    return Err(Error::Invalid(
      "remote bundle file exceeds its size limit".into(),
    ));
  }
  Ok(bytes)
}

fn trusted_url(url: &reqwest::Url) -> bool {
  url.scheme() == "https"
    && url.port_or_known_default() == Some(443)
    && url.username().is_empty()
    && url.password().is_none()
    && matches!(
      url.host_str(),
      Some(
        "github.com"
          | "release-assets.githubusercontent.com"
          | "objects.githubusercontent.com"
          | "github-releases.githubusercontent.com"
      )
    )
}

fn download_error(error: reqwest::Error) -> Error {
  Error::Download(error.without_url().to_string())
}

async fn download(client: &reqwest::Client, url: &str, maximum: usize) -> Result<Vec<u8>, Error> {
  let response = client.get(url).send().await.map_err(download_error)?;
  if response.status() == reqwest::StatusCode::NOT_FOUND {
    return Err(Error::NotAvailable(
      "the matching remote component release is not published".into(),
    ));
  }
  let mut response = response.error_for_status().map_err(download_error)?;
  let length = response.content_length();
  if length.is_some_and(|length| length > maximum as u64) {
    return Err(Error::Invalid(
      "remote bundle download exceeds its size limit".into(),
    ));
  }
  let mut bytes = Vec::new();
  while let Some(chunk) = response.chunk().await.map_err(download_error)? {
    if chunk.len() > maximum.saturating_sub(bytes.len()) {
      return Err(Error::Invalid(
        "remote bundle download exceeds its size limit".into(),
      ));
    }
    bytes.extend_from_slice(&chunk);
  }
  if length.is_some_and(|length| length != bytes.len() as u64) {
    return Err(Error::Invalid("truncated remote bundle download".into()));
  }
  Ok(bytes)
}

#[cfg(test)]
mod tests;
