//! Resolve and verify desktop packages from a published component channel.

use super::{Error, MAX_BUNDLE_SET_BYTES, RELEASE_ROOT, published, release_http_client};
use ctl_core::component_update::BundleChannel;
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;

mod nightly;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopTarget {
  MacX64,
  MacArm64,
  LinuxX64,
  LinuxArm64,
}

impl DesktopTarget {
  /// # Errors
  /// Returns an error when releases do not include the updater's platform.
  pub fn native() -> io::Result<Self> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
      ("macos", "x86_64") => Ok(Self::MacX64),
      ("macos", "aarch64") => Ok(Self::MacArm64),
      ("linux", "x86_64") => Ok(Self::LinuxX64),
      ("linux", "aarch64") => Ok(Self::LinuxArm64),
      (os, arch) => Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("no desktop package is available for {os} {arch}"),
      )),
    }
  }

  const fn is_macos(self) -> bool {
    matches!(self, Self::MacX64 | Self::MacArm64)
  }

  const fn triple(self) -> &'static str {
    match self {
      Self::MacX64 => "x86_64-apple-darwin",
      Self::MacArm64 => "aarch64-apple-darwin",
      Self::LinuxX64 => "x86_64-unknown-linux-gnu",
      Self::LinuxArm64 => "aarch64-unknown-linux-gnu",
    }
  }

  const fn package_format(self) -> &'static str {
    if self.is_macos() {
      "app.tar.gz"
    } else {
      "AppImage"
    }
  }

  const fn signing_options(self) -> &'static [DesktopSigning] {
    if self.is_macos() {
      &[DesktopSigning::Signed, DesktopSigning::Unsigned]
    } else {
      &[DesktopSigning::NotApplicable]
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSigning {
  Signed,
  Unsigned,
  NotApplicable,
}

impl DesktopSigning {
  const fn suffix(self) -> &'static str {
    match self {
      Self::Unsigned => "-unsigned",
      Self::Signed | Self::NotApplicable => "",
    }
  }
}

pub struct DesktopRelease {
  pub app_version: String,
  pub bundle_id: String,
  source: PackageSource,
  asset: Asset,
  signing: DesktopSigning,
}

enum PackageSource {
  Release { client: Client, url: String },
  Workflow(nightly::DownloadedArtifact),
}

impl DesktopRelease {
  #[must_use]
  pub fn asset_name(&self) -> &str {
    &self.asset.name
  }

  #[must_use]
  pub const fn signing(&self) -> DesktopSigning {
    self.signing
  }

  /// Compares a local package to the verified release digest.
  ///
  /// # Errors
  /// Reports errors opening or reading a package. The path must be a regular file.
  pub fn matches(&self, path: &Path) -> io::Result<bool> {
    let file = open_package(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_PACKAGE_BYTES {
      return Ok(false);
    }
    let mut digest = Sha256::new();
    let size = io::copy(&mut file.take(MAX_PACKAGE_BYTES + 1), &mut digest)?;
    Ok(size > 0 && size <= MAX_PACKAGE_BYTES && self.asset.matches(digest))
  }

  /// Streams a verified package to a new file in the caller's private staging directory.
  ///
  /// # Errors
  /// Rejects unavailable packages, existing destinations, and invalid downloads.
  pub async fn download(&self, destination: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(destination)?;
    let mut partial = PartialDownload(Some(destination));
    match &self.source {
      PackageSource::Release { client, url } => {
        let response = client.get(url).send().await.map_err(network_error)?;
        if response.status() == StatusCode::NOT_FOUND {
          return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("desktop package {} is unavailable", self.asset.name),
          ));
        }
        let response = response.error_for_status().map_err(network_error)?;
        write_verified(response, &mut file, &self.asset).await?;
      }
      PackageSource::Workflow(directory) => {
        directory.copy_verified(&self.asset, &mut file)?;
      }
    }
    file.sync_all()?;
    partial.0 = None;
    Ok(())
  }
}

fn open_package(path: &Path) -> io::Result<File> {
  use rustix::fs::{Mode, OFlags};

  let file = File::from(rustix::fs::open(
    path,
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
    Mode::empty(),
  )?);
  if !file.metadata()?.is_file() {
    return Err(invalid("desktop package must be a regular file"));
  }
  Ok(file)
}

/// Resolves the desktop package for the exact publication named by a channel.
///
/// # Errors
/// Reports unavailable desktop assets and metadata that fails verification.
pub async fn latest(channel: BundleChannel, target: DesktopTarget) -> io::Result<DesktopRelease> {
  if channel == BundleChannel::Nightly {
    return nightly::latest(target).await;
  }
  let stable = published::check(channel).await.map_err(io::Error::other)?;
  let root = format!("{RELEASE_ROOT}/v{}", stable.app_version);
  let client = release_http_client().map_err(io::Error::other)?;
  let (signing, asset) = discover_asset(
    target,
    &stable.bundle_id,
    &stable.git_revision,
    async |name| super::download(&client, &format!("{root}/{name}"), MAX_BUNDLE_SET_BYTES).await,
  )
  .await?;
  Ok(DesktopRelease {
    app_version: stable.app_version,
    bundle_id: stable.bundle_id,
    source: PackageSource::Release {
      url: format!("{root}/{}", asset.name),
      client,
    },
    asset,
    signing,
  })
}

async fn discover_asset(
  target: DesktopTarget,
  bundle_id: &str,
  git_revision: &str,
  mut fetch: impl AsyncFnMut(String) -> Result<Vec<u8>, Error>,
) -> io::Result<(DesktopSigning, Asset)> {
  for &signing in target.signing_options() {
    let name = format!("desktop-{}{}.json", target.triple(), signing.suffix());
    match fetch(name).await {
      Ok(bytes) => {
        let asset = verified_asset(&bytes, target, signing, bundle_id, git_revision)?;
        return Ok((signing, asset));
      }
      Err(Error::NotAvailable(_)) => {}
      Err(error) => return Err(io::Error::other(error)),
    }
  }
  Err(io::Error::new(
    io::ErrorKind::NotFound,
    format!(
      "desktop package for {} is unavailable in v{bundle_id}",
      target.triple()
    ),
  ))
}

#[derive(Deserialize)]
struct Manifest {
  schema_version: u32,
  target: String,
  bundle_id: String,
  git_revision: String,
  signing_mode: DesktopSigning,
  assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
  name: String,
  sha256: String,
}

impl Asset {
  fn matches(&self, digest: Sha256) -> bool {
    format!("{:x}", digest.finalize()) == self.sha256
  }
}

fn verified_asset(
  bytes: &[u8],
  target: DesktopTarget,
  signing: DesktopSigning,
  bundle_id: &str,
  git_revision: &str,
) -> io::Result<Asset> {
  let manifest = parse_manifest(bytes, target, bundle_id, git_revision)?;
  if manifest.signing_mode != signing {
    return Err(invalid("unexpected desktop release signing mode"));
  }
  select_asset(manifest.assets, target, bundle_id, signing.suffix())
}

fn parse_manifest(
  bytes: &[u8],
  target: DesktopTarget,
  bundle_id: &str,
  git_revision: &str,
) -> io::Result<Manifest> {
  let manifest: Manifest =
    serde_json::from_slice(bytes).map_err(|_| invalid("invalid desktop release manifest JSON"))?;
  if manifest.schema_version != 1
    || manifest.target != target.triple()
    || manifest.bundle_id != bundle_id
    || manifest.git_revision != git_revision
  {
    return Err(invalid(
      "desktop manifest does not match the selected published build",
    ));
  }
  Ok(manifest)
}

fn select_asset(
  assets: Vec<Asset>,
  target: DesktopTarget,
  bundle_id: &str,
  suffix: &str,
) -> io::Result<Asset> {
  let expected_name = format!(
    "ctmux-{bundle_id}-{}{suffix}.{}",
    target.triple(),
    target.package_format()
  );
  let mut matching = assets
    .into_iter()
    .filter(|asset| asset.name == expected_name);
  let asset = matching
    .next()
    .ok_or_else(|| invalid("desktop manifest is missing the installable package"))?;
  if matching.next().is_some() {
    return Err(invalid("desktop manifest contains duplicate package names"));
  }
  if asset.sha256.len() != 64
    || !asset
      .sha256
      .bytes()
      .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
  {
    return Err(invalid(
      "desktop manifest contains an invalid package checksum",
    ));
  }
  Ok(asset)
}

async fn write_verified(mut response: Response, file: &mut File, asset: &Asset) -> io::Result<()> {
  let length = response.content_length();
  if length.is_some_and(|size| size > MAX_PACKAGE_BYTES) {
    return Err(invalid("desktop package exceeds its download size limit"));
  }
  let mut digest = Sha256::new();
  let mut received = 0;
  while let Some(chunk) = response.chunk().await.map_err(|error| {
    let error = network_error(error);
    io::Error::new(
      error.kind(),
      format!("{error} after receiving {received} bytes"),
    )
  })? {
    received += chunk.len() as u64;
    if received > MAX_PACKAGE_BYTES {
      return Err(invalid("desktop package exceeds its download size limit"));
    }
    digest.update(&chunk);
    file.write_all(&chunk)?;
  }
  if received == 0 || length.is_some_and(|length| length != received) {
    return Err(invalid("incomplete desktop package download"));
  }
  if !asset.matches(digest) {
    return Err(invalid("desktop package SHA-256 checksum mismatch"));
  }
  Ok(())
}

struct PartialDownload<'a>(Option<&'a Path>);

impl Drop for PartialDownload<'_> {
  fn drop(&mut self) {
    if let Some(path) = self.0 {
      let _ = fs::remove_file(path);
    }
  }
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

fn network_error(error: reqwest::Error) -> io::Error {
  if error.is_timeout() {
    return io::Error::new(
      io::ErrorKind::TimedOut,
      "desktop package download timed out",
    );
  }
  io::Error::other(format!(
    "could not download desktop package: {}",
    error.without_url()
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::{Value, json};
  use std::collections::VecDeque;

  const VERSION: &str = "0.1.3";
  const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

  fn manifest(target: DesktopTarget, packages: &[&str], signing_mode: &str) -> Value {
    let suffix = if signing_mode == "unsigned" {
      "-unsigned"
    } else {
      ""
    };
    let assets: Vec<_> = packages
      .iter()
      .map(|package| {
        json!({
          "name": format!("ctmux-{VERSION}-{}{suffix}.{package}", target.triple()),
          "sha256": "a".repeat(64)
        })
      })
      .collect();
    json!({
      "schema_version": 1,
      "target": target.triple(),
      "bundle_id": VERSION,
      "git_revision": REVISION,
      "signing_mode": signing_mode,
      "assets": assets
    })
  }

  fn verify(value: &Value, target: DesktopTarget) -> io::Result<Asset> {
    verified_asset(
      &serde_json::to_vec(value).unwrap(),
      target,
      target.signing_options()[0],
      VERSION,
      REVISION,
    )
  }

  #[test]
  fn selects_the_installable_format_for_the_exact_desktop_target() {
    let cases: &[(DesktopTarget, &[&str], &str, &str)] = &[
      (
        DesktopTarget::MacArm64,
        &["dmg", "app.tar.gz"],
        "signed",
        "app.tar.gz",
      ),
      (
        DesktopTarget::MacX64,
        &["dmg", "app.tar.gz"],
        "signed",
        "app.tar.gz",
      ),
      (
        DesktopTarget::LinuxArm64,
        &["deb", "rpm", "AppImage"],
        "not_applicable",
        "AppImage",
      ),
      (
        DesktopTarget::LinuxX64,
        &["deb", "rpm", "AppImage"],
        "not_applicable",
        "AppImage",
      ),
    ];
    for &(target, formats, signing_mode, extension) in cases {
      let value = manifest(target, formats, signing_mode);
      assert_eq!(
        verify(&value, target).unwrap().name,
        format!("ctmux-{VERSION}-{}.{extension}", target.triple()),
      );
    }
  }

  #[test]
  fn rejects_an_unmatched_manifest_or_unsigned_data_in_the_signed_manifest() {
    let target = DesktopTarget::MacArm64;
    let value = manifest(target, &["dmg", "app.tar.gz"], "signed");
    assert!(verify(&value, DesktopTarget::MacX64).is_err());
    for (key, invalid_value) in [
      ("schema_version", json!(2)),
      ("bundle_id", json!("0.1.2")),
      ("git_revision", json!("0".repeat(40))),
      ("signing_mode", json!("unsigned")),
    ] {
      let mut bad = value.clone();
      bad[key] = invalid_value;
      assert!(verify(&bad, target).is_err(), "{key}");
    }
  }

  #[test]
  fn rejects_missing_duplicate_or_unsafe_assets_and_invalid_digests() {
    let target = DesktopTarget::LinuxX64;
    let value = manifest(target, &["AppImage", "deb", "rpm"], "not_applicable");
    let mut missing = value.clone();
    missing["assets"].as_array_mut().unwrap().remove(0);
    assert!(verify(&missing, target).is_err());
    let mut duplicate = value.clone();
    duplicate["assets"][1] = duplicate["assets"][0].clone();
    assert!(verify(&duplicate, target).is_err());
    let mut unsafe_name = value.clone();
    unsafe_name["assets"][0]["name"] = json!("../ctmux.AppImage");
    assert!(verify(&unsafe_name, target).is_err());
    let mut invalid_digest = value;
    invalid_digest["assets"][0]["sha256"] = json!("g".repeat(64));
    assert!(verify(&invalid_digest, target).is_err());
  }

  #[test]
  fn accepts_a_manifest_with_only_the_installable_asset_and_extra_metadata() {
    let target = DesktopTarget::LinuxX64;
    let mut value = manifest(target, &["AppImage"], "not_applicable");
    value["asset_count"] = json!(1);
    value["assets"][0]["size"] = json!(1);
    assert!(verify(&value, target).is_ok());
  }

  #[tokio::test]
  async fn prefers_signed_and_only_falls_back_when_the_signed_manifest_is_absent() {
    let target = DesktopTarget::MacArm64;
    let signed = "desktop-aarch64-apple-darwin.json";
    let unsigned = "desktop-aarch64-apple-darwin-unsigned.json";
    let bytes = |mode| serde_json::to_vec(&manifest(target, &["app.tar.gz"], mode)).unwrap();
    let missing = || Error::NotAvailable("missing manifest".into());
    for (responses, expected_requests, expected_signing) in [
      (
        vec![Ok(bytes("signed")), Ok(bytes("unsigned"))],
        &[signed][..],
        Some(DesktopSigning::Signed),
      ),
      (
        vec![Err(missing()), Ok(bytes("unsigned"))],
        &[signed, unsigned][..],
        Some(DesktopSigning::Unsigned),
      ),
      (
        vec![Ok(bytes("unsigned")), Ok(bytes("unsigned"))],
        &[signed][..],
        None,
      ),
      (
        vec![
          Err(Error::Download("network error".into())),
          Ok(bytes("unsigned")),
        ],
        &[signed][..],
        None,
      ),
    ] {
      let mut responses = VecDeque::from(responses);
      let mut requests = Vec::new();
      let result = discover_asset(target, VERSION, REVISION, async |name| {
        requests.push(name);
        responses.pop_front().unwrap()
      })
      .await;
      assert_eq!(requests, expected_requests);
      assert_eq!(result.ok().map(|(signing, _)| signing), expected_signing);
    }
  }

  #[tokio::test]
  async fn requires_the_official_unsigned_asset_name_and_reports_absent_variants() {
    let target = DesktopTarget::MacArm64;
    let unsigned = DesktopSigning::Unsigned;
    let mut value = manifest(target, &["app.tar.gz"], "unsigned");
    let bytes = serde_json::to_vec(&value).unwrap();
    assert_eq!(
      verified_asset(&bytes, target, unsigned, VERSION, REVISION)
        .unwrap()
        .name,
      "ctmux-0.1.3-aarch64-apple-darwin-unsigned.app.tar.gz"
    );
    value["assets"][0]["name"] = json!("ctmux-0.1.3-aarch64-apple-darwin.app.tar.gz");
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(verified_asset(&bytes, target, unsigned, VERSION, REVISION).is_err());
    let result = discover_asset(target, VERSION, REVISION, async |_| {
      Err(Error::NotAvailable("missing manifest".into()))
    })
    .await;
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::NotFound));
  }
}
