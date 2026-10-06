//! Provenance for an agent-only installation. The manifest describes its source
//! build; it makes no claim that the source's companion binaries were installed.

use super::{MAX_FILE_BYTES, MAX_MANIFEST_BYTES, Manifest, digest, invalid};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

pub const SOURCE_FILE: &str = "agent-source.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSource {
  pub schema_version: u16,
  pub source_bundle: Manifest,
}

impl AgentSource {
  /// Checks only the agent bytes against a verified complete source build.
  ///
  /// # Errors
  /// Rejects invalid source metadata or changed agent bytes.
  pub fn verify(&self, bytes: &[u8]) -> io::Result<()> {
    self.source_bundle.validate()?;
    if self.schema_version != 1
      || bytes.is_empty()
      || bytes.len() > MAX_FILE_BYTES
      || digest(bytes) != self.source_bundle.files["ctl-agent"].sha256
    {
      return Err(invalid("agent does not match its complete source build"));
    }
    Ok(())
  }

  /// Reads the provenance beside an installed agent, without inspecting or
  /// executing its retained companions.
  ///
  /// # Errors
  /// Rejects links, excessive data, invalid metadata or changed agent bytes.
  pub fn open(directory: &Path) -> io::Result<Self> {
    let source = read(&directory.join(SOURCE_FILE), MAX_MANIFEST_BYTES)?;
    let source: Self = serde_json::from_slice(&source).map_err(io::Error::other)?;
    source.verify(&read(&directory.join("ctl-agent"), MAX_FILE_BYTES)?)?;
    Ok(source)
  }
}

fn read(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
  // Use the same private, owner-only regular-file checks as the bundle store.
  super::filesystem::read(path, maximum)
}
