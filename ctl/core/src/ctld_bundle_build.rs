//! Cargo build-script support for executables that can embed a ctld helper.

use crate::component::{ProtocolInfo, protocols_are_valid};
#[cfg(feature = "ctld-bundle-build")]
use crate::development;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
#[cfg(feature = "ctld-bundle-build")]
use std::env;
use std::fs;
use std::io;
use std::path::Path;
#[cfg(feature = "ctld-bundle-build")]
use std::path::PathBuf;

/// Generates resolver inputs in the calling executable package's Cargo output directory.
///
/// # Panics
/// Panics when called outside a Cargo build script or when build inputs are invalid.
#[cfg(feature = "ctld-bundle-build")]
pub fn write(repository_package: &Path) {
  println!("cargo:rerun-if-changed=build.rs");
  println!("cargo:rerun-if-env-changed=CTL_BUNDLED_CTLD_DIR");
  println!("cargo:rerun-if-env-changed=CTL_BUNDLED_CTLD_MODE");

  let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"));
  let source = env::var_os("CTL_BUNDLED_CTLD_DIR").map(PathBuf::from);
  let target = env::var("TARGET").expect("Cargo did not set TARGET");
  let development = if source.is_none() {
    let manifest = PathBuf::from(
      env::var_os("CARGO_MANIFEST_DIR").expect("Cargo did not set CARGO_MANIFEST_DIR"),
    );
    development::context(&manifest, repository_package, &output, &target)
      .expect("could not determine local development helper provenance")
  } else {
    None
  };
  development::write(&output, development.as_ref())
    .expect("could not write local development helper provenance");

  let (mode, generated_mode) = match env::var("CTL_BUNDLED_CTLD_MODE") {
    Err(env::VarError::NotPresent) => (Mode::Signed, "Signed"),
    Ok(value) if value == "signed" => (Mode::Signed, "Signed"),
    Ok(value) if value == "development" => (Mode::Development, "Development"),
    _ => panic!("CTL_BUNDLED_CTLD_MODE must be signed or development"),
  };
  let bundle = if let Some(source) = source {
    let version = env::var("CARGO_PKG_VERSION").expect("Cargo did not set CARGO_PKG_VERSION");
    stage(&source, &output, &version, &target, mode).expect("invalid bundled ctld payload");
    format!(
      concat!(
        "Some(ctl_client::ctld::EmbeddedBundle {{\n",
        "  manifest: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ctld-manifest.json\")),\n",
        "  archive: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ctld.app.tar.gz\")),\n",
        "  mode: ctl_client::ctld::BundleMode::{mode},\n",
        "}})"
      ),
      mode = generated_mode,
    )
  } else {
    "None".to_owned()
  };
  fs::write(
    output.join("bundled_ctld.rs"),
    format!("const BUNDLED_CTLD: Option<ctl_client::ctld::EmbeddedBundle> = {bundle};\n"),
  )
  .expect("could not write embedded helper inputs");
}

#[derive(Clone, Copy)]
enum Mode {
  Signed,
  Development,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Development {
  source_fingerprint: String,
  // Bind this flag at installation, while permitting iterative local builds.
  #[serde(rename = "dirty")]
  _dirty: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
  schema_version: u16,
  component: String,
  app_version: String,
  bundle_id: String,
  git_revision: String,
  target: String,
  bundle_identifier: String,
  team_identifier: String,
  signing_mode: String,
  notarized: bool,
  archive: String,
  sha256: String,
  archive_size: u64,
  protocols: Vec<ProtocolInfo>,
  #[serde(default, deserialize_with = "development_identity")]
  development: Option<Development>,
}

fn development_identity<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<Development>, D::Error> {
  Development::deserialize(deserializer).map(Some)
}

/// Check payload identity/hash and snapshot it into Cargo's build directory.
/// The release pipeline and runtime installer perform Apple's trust checks.
fn stage(source: &Path, output: &Path, version: &str, target: &str, mode: Mode) -> io::Result<()> {
  if !matches!(target, "aarch64-apple-darwin" | "x86_64-apple-darwin") {
    return Err(invalid("bundled ctld currently requires a macOS target"));
  }
  let manifest_path = source.join(format!("ctld-{target}.json"));
  println!("cargo:rerun-if-changed={}", manifest_path.display());
  let bytes = read_bounded(&manifest_path, 16 * 1024)?;
  let manifest: Manifest = serde_json::from_slice(&bytes).map_err(invalid)?;
  if manifest.schema_version != 1
    || manifest.component != "ctld"
    || manifest.app_version != version
    || manifest.target != target
    || manifest.bundle_identifier != "dev.tokn-ai.ctl.ctld"
    || manifest.team_identifier.len() != 10
    || !manifest
      .team_identifier
      .bytes()
      .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    || !lower_hex(&manifest.git_revision, 40)
    || manifest.archive != format!("ctld-{version}-{target}.app.tar.gz")
    || !lower_hex(&manifest.sha256, 64)
    || manifest.archive_size == 0
    || manifest.archive_size > 128 * 1024 * 1024
    || !protocols_are_valid(&manifest.protocols)
    || ["ctld", "ctld_lifecycle", "ctld_helper"]
      .iter()
      .any(|name| {
        !manifest
          .protocols
          .iter()
          .any(|protocol| protocol.name == *name)
      })
  {
    return Err(invalid("ctld payload does not match this CLI"));
  }
  let policy_matches = match mode {
    Mode::Signed => {
      manifest.bundle_id == version
        && manifest.signing_mode == "signed"
        && manifest.notarized
        && manifest.development.is_none()
    }
    Mode::Development => {
      manifest.bundle_id == format!("dev.{}", manifest.sha256)
        && manifest.signing_mode == "development"
        && !manifest.notarized
        && manifest
          .development
          .as_ref()
          .is_some_and(|build| lower_hex(&build.source_fingerprint, 64))
    }
  };
  if !policy_matches {
    return Err(invalid(
      "ctld payload does not match the requested signing mode",
    ));
  }
  let archive_path = source.join(&manifest.archive);
  println!("cargo:rerun-if-changed={}", archive_path.display());
  let archive = read_bounded(&archive_path, 128 * 1024 * 1024)?;
  if archive.len() as u64 != manifest.archive_size
    || hex::encode(Sha256::digest(&archive)) != manifest.sha256
  {
    return Err(invalid("bundled ctld archive size or checksum mismatch"));
  }
  // Embed these snapshots, rather than input paths that another build can replace.
  fs::write(output.join("ctld-manifest.json"), bytes)?;
  fs::write(output.join("ctld.app.tar.gz"), archive)?;
  Ok(())
}

fn lower_hex(value: &str, length: usize) -> bool {
  value.len() == length
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
  use std::io::Read as _;
  #[cfg(unix)]
  let file = fs::File::from(rustix::fs::open(
    path,
    rustix::fs::OFlags::RDONLY
      | rustix::fs::OFlags::NOFOLLOW
      | rustix::fs::OFlags::NONBLOCK
      | rustix::fs::OFlags::CLOEXEC,
    rustix::fs::Mode::empty(),
  )?);
  #[cfg(not(unix))]
  let file = fs::File::open(path)?;
  if !file.metadata()?.is_file() {
    return Err(invalid("ctld payload must contain regular files"));
  }
  let mut bytes = Vec::new();
  file.take(limit + 1).read_to_end(&mut bytes)?;
  if bytes.len() as u64 > limit {
    return Err(invalid("oversized ctld payload"));
  }
  Ok(bytes)
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests;
