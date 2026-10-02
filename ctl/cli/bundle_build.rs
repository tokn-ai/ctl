use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io;
use std::path::Path;

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
}

/// Check payload identity/hash and snapshot it into Cargo's build directory.
/// The release pipeline and runtime installer perform Apple's trust checks.
pub fn stage(source: &Path, output: &Path, version: &str, target: &str) -> io::Result<()> {
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
    || manifest.bundle_id != version
    || manifest.target != target
    || manifest.bundle_identifier != "dev.tokn-ai.ctl.ctld"
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
      .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    || manifest.archive != format!("ctld-{version}-{target}.app.tar.gz")
    || manifest.sha256.len() != 64
    || manifest.archive_size == 0
    || manifest.archive_size > 128 * 1024 * 1024
  {
    return Err(invalid("ctld payload does not match this signed release"));
  }
  let archive_path = source.join(&manifest.archive);
  println!("cargo:rerun-if-changed={}", archive_path.display());
  let archive = read_bounded(&archive_path, 128 * 1024 * 1024)?;
  if archive.len() as u64 != manifest.archive_size
    || format!("{:x}", Sha256::digest(&archive)) != manifest.sha256
  {
    return Err(invalid("bundled ctld archive size or checksum mismatch"));
  }
  // Embed these snapshots, rather than input paths that another build can replace.
  fs::write(output.join("ctld-manifest.json"), bytes)?;
  fs::write(output.join("ctld.app.tar.gz"), archive)?;
  Ok(())
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
