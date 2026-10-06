//! Immutable complete builds shared by local execution and remote upload.
//! Selection is an explicit transaction; discovery never searches for a newer build.

mod filesystem;
#[cfg(test)]
mod tests;

use crate::component::{ComponentInfo, protocols_match};
use crate::protocol::ProtocolVersion;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

pub const COMPONENTS: [&str; 4] = ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"];
pub const MAX_FILE_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;
pub const MANIFEST_FILE: &str = "bundle.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
  Ci,
  Release,
  Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
  Local,
  Upload,
}

impl Purpose {
  fn name(self) -> &'static str {
    match self {
      Self::Local => "local",
      Self::Upload => "upload",
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInfo {
  pub sha256: String,
  pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
  pub schema_version: u16,
  /// Content identity of the entire manifest and payload, not a sortable version.
  pub bundle_id: String,
  pub target_triple: String,
  pub source: Source,
  /// Original publisher identity, retained for older compatible agents.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub distribution_id: Option<String>,
  #[serde(deserialize_with = "unique_components")]
  pub components: BTreeMap<String, ComponentInfo>,
  #[serde(deserialize_with = "unique_files")]
  pub files: BTreeMap<String, FileInfo>,
}

fn unique_components<'de, D: serde::Deserializer<'de>>(
  d: D,
) -> Result<BTreeMap<String, ComponentInfo>, D::Error> {
  unique_map(d, 4)
}
fn unique_files<'de, D: serde::Deserializer<'de>>(
  d: D,
) -> Result<BTreeMap<String, FileInfo>, D::Error> {
  unique_map(d, 512)
}
fn unique_map<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
  d: D,
  limit: usize,
) -> Result<BTreeMap<String, T>, D::Error> {
  struct Visitor<T>(usize, std::marker::PhantomData<T>);
  impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
    type Value = BTreeMap<String, T>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.write_str("a bounded map with unique keys")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(
      self,
      mut access: A,
    ) -> Result<Self::Value, A::Error> {
      let mut map = BTreeMap::new();
      while let Some((key, value)) = access.next_entry::<String, T>()? {
        if map.insert(key, value).is_some() || map.len() > self.0 {
          return Err(serde::de::Error::custom(
            "duplicate or excessive bundle entries",
          ));
        }
      }
      Ok(map)
    }
  }
  d.deserialize_map(Visitor(limit, std::marker::PhantomData))
}

impl Manifest {
  /// Constructs one complete build, binding all executable and package bytes.
  ///
  /// # Errors
  /// Rejects mixed builds, missing components, invalid dependencies or unsafe files.
  pub fn new(
    target: &str,
    source: Source,
    components: BTreeMap<String, ComponentInfo>,
    files: &BTreeMap<String, Vec<u8>>,
  ) -> io::Result<Self> {
    let mut manifest = Self {
      schema_version: 1,
      bundle_id: String::new(),
      target_triple: target.into(),
      source,
      distribution_id: None,
      components,
      files: files
        .iter()
        .map(|(name, bytes)| {
          (
            name.clone(),
            FileInfo {
              sha256: digest(bytes),
              executable: COMPONENTS.contains(&name.as_str())
                || name == "ctld.app/Contents/MacOS/ctld",
            },
          )
        })
        .collect(),
    };
    manifest.bundle_id = manifest.content_id()?;
    manifest.validate()?;
    manifest.verify_files(files)?;
    Ok(manifest)
  }

  fn content_id(&self) -> io::Result<String> {
    let mut identity = self.clone();
    identity.bundle_id.clear();
    Ok(digest(
      &serde_json::to_vec(&identity).map_err(io::Error::other)?,
    ))
  }

  /// Retains the original published identity without changing its component bytes.
  ///
  /// # Errors
  /// Rejects unsafe publisher IDs or an invalid complete bundle.
  pub fn with_distribution_id(mut self, id: &str) -> io::Result<Self> {
    self.distribution_id = Some(id.into());
    self.bundle_id = self.content_id()?;
    self.validate()?;
    Ok(self)
  }

  /// Preserves executable files inside a complete helper package.
  ///
  /// # Errors
  /// Rejects names absent from the verified payload or invalid bundle metadata.
  pub fn with_executables(mut self, names: &[String]) -> io::Result<Self> {
    for name in names {
      self
        .files
        .get_mut(name)
        .ok_or_else(|| invalid("unknown executable bundle file"))?
        .executable = true;
    }
    self.bundle_id = self.content_id()?;
    self.validate()?;
    Ok(self)
  }

  /// Validates the complete build and every internal companion protocol edge.
  ///
  /// # Errors
  /// Rejects malformed metadata or an identity that does not bind this manifest.
  pub fn validate(&self) -> io::Result<()> {
    validate_target(&self.target_triple)?;
    if serde_json::to_vec(self).map_err(io::Error::other)?.len() > MAX_MANIFEST_BYTES
      || self.schema_version != 1
      || self.distribution_id.as_ref().is_some_and(|id| {
        id.is_empty()
          || id.len() > 128
          || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
      })
      || self.bundle_id != self.content_id()?
      || self.components.len() != COMPONENTS.len()
      || COMPONENTS
        .iter()
        .any(|name| !self.components.contains_key(*name))
      || self.files.len() > 512
      || self.files.keys().any(|name| !safe_file(name))
      || COMPONENTS
        .iter()
        .any(|name| !self.files.get(*name).is_some_and(|file| file.executable))
      || self.files.values().any(|file| !valid_digest(&file.sha256))
      || self
        .files
        .get("ctld.app/Contents/MacOS/ctld")
        .is_some_and(|app| {
          self
            .files
            .get("ctld")
            .is_none_or(|binary| app.sha256 != binary.sha256)
        })
    {
      return Err(invalid("invalid complete component bundle"));
    }
    let first = &self.components["ctl-agent"].build;
    if !first.is_valid()
      || first.source_revision.is_none()
      || self.components.values().any(|component| {
        !component.is_valid()
          || component.build.version != first.version
          || component.build.source_revision != first.source_revision
          || component.build.source_fingerprint != first.source_fingerprint
          || component.build.dirty != first.dirty
      })
      || (first.dirty && self.source != Source::Local)
    {
      return Err(invalid(
        "bundle components must come from the same identified build",
      ));
    }
    for (consumer, provider, protocol) in [
      ("ctl-agent", "ctld", "ctld"),
      ("ctl-agent", "ctmuxd", "ctmux"),
      ("ctl-agent", "ctmuxd", "ctmux_control"),
      ("ctl-agent", "ctl-taskd", "task"),
      ("ctl-agent", "ctl-taskd", "task_control"),
      ("ctl-taskd", "ctmuxd", "ctmux"),
      ("ctl-taskd", "ctmuxd", "ctmux_control"),
    ] {
      let client = self.components[consumer]
        .protocols
        .iter()
        .find(|entry| entry.name == protocol);
      let server = self.components[provider]
        .protocols
        .iter()
        .find(|entry| entry.name == protocol);
      if client
        .zip(server)
        .is_none_or(|(client, server)| server.negotiate(&client.supported_versions).is_none())
      {
        return Err(invalid(
          "bundle components have incompatible companion protocols",
        ));
      }
    }
    Ok(())
  }

  /// Checks supplied bytes against exactly the files named by the manifest.
  ///
  /// # Errors
  /// Rejects missing, excessive, oversized or changed payload files.
  pub fn verify_files(&self, files: &BTreeMap<String, Vec<u8>>) -> io::Result<()> {
    let mut total = 0usize;
    if files.len() != self.files.len() {
      return Err(invalid("incomplete bundle payload"));
    }
    for (name, file) in &self.files {
      let bytes = files
        .get(name)
        .ok_or_else(|| invalid("missing bundle file"))?;
      total = total.saturating_add(bytes.len());
      if bytes.is_empty()
        || bytes.len() > MAX_FILE_BYTES
        || total > MAX_TOTAL_BYTES
        || digest(bytes) != file.sha256
      {
        return Err(invalid(
          "bundle payload failed checksum or size verification",
        ));
      }
    }
    Ok(())
  }

  #[must_use]
  pub fn same_component(&self, component: &str, actual: &ComponentInfo) -> bool {
    self.components.get(component).is_some_and(|expected| {
      expected.build == actual.build && protocols_match(&expected.protocols, &actual.protocols)
    })
  }
}

#[derive(Debug, Clone)]
pub struct Bundle {
  pub directory: PathBuf,
  pub manifest: Manifest,
}

impl Bundle {
  /// Opens a directory pinned by its executable, verifying all payload bytes.
  ///
  /// # Errors
  /// Rejects invalid manifests, unsafe files or changed payload bytes.
  pub fn open(directory: &Path) -> io::Result<Self> {
    let manifest = filesystem::read_manifest(directory)?;
    let bundle = Self {
      directory: directory.to_owned(),
      manifest,
    };
    filesystem::verify_payload(&bundle.directory, &bundle.manifest)?;
    Ok(bundle)
  }

  /// Reads verified bytes without executing foreign-target components.
  ///
  /// # Errors
  /// Rejects unsafe or changed files.
  pub fn read_files(&self) -> io::Result<BTreeMap<String, Vec<u8>>> {
    filesystem::payload(self)
  }

  #[must_use]
  pub fn executable(&self, component: &str) -> Option<PathBuf> {
    self
      .manifest
      .components
      .contains_key(component)
      .then(|| self.directory.join(component))
  }
}

#[derive(Debug, Clone)]
pub struct Store {
  root: PathBuf,
}

impl Store {
  /// No filesystem work is performed until an explicit import or selection.
  #[must_use]
  pub fn new(home: &Path) -> Self {
    Self {
      root: home.join(".tokn/ctl/components"),
    }
  }

  #[must_use]
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Reads only the explicitly selected immutable bundle, verifying its payload.
  ///
  /// # Errors
  /// Invalid selections and changed bytes fail instead of selecting another build.
  pub fn selected(&self, purpose: Purpose, target: &str) -> io::Result<Option<Bundle>> {
    filesystem::selected(self, purpose, target)
  }

  /// Lists verified stored bundles for one target without changing selection.
  ///
  /// # Errors
  /// Rejects unsafe or corrupt stored entries.
  pub fn list(&self, target: &str) -> io::Result<Vec<Bundle>> {
    filesystem::list(self, target)
  }

  /// Loads one verified stored bundle by its target and content identity.
  ///
  /// # Errors
  /// Rejects unsafe identities, absent bundles or changed payloads.
  pub fn get(&self, target: &str, bundle_id: &str) -> io::Result<Bundle> {
    validate_target(target)?;
    if !valid_digest(bundle_id) {
      return Err(invalid("invalid component bundle identity"));
    }
    filesystem::load(self, target, bundle_id)
  }

  /// Publishes immutable bytes; it does not select or start a component.
  ///
  /// # Errors
  /// Rejects invalid data, unsafe paths, concurrent writers or changed existing entries.
  pub fn publish(
    &self,
    manifest: &Manifest,
    files: &BTreeMap<String, Vec<u8>>,
  ) -> io::Result<Bundle> {
    filesystem::publish(self, manifest, files)
  }

  /// Atomically selects one already verified complete bundle for a purpose.
  ///
  /// # Errors
  /// Rejects changed bundles, non-native local selections or unsigned local macOS payloads.
  pub fn select(&self, purpose: Purpose, bundle: &Bundle) -> io::Result<()> {
    filesystem::select(self, purpose, bundle, false)
  }

  /// Initializes a missing selection, preserving any concurrent explicit choice.
  ///
  /// # Errors
  /// Rejects damaged existing selections or invalid bundle payloads.
  pub fn select_if_unset(&self, purpose: Purpose, bundle: &Bundle) -> io::Result<Bundle> {
    filesystem::select(self, purpose, bundle, true)?;
    self
      .selected(purpose, &bundle.manifest.target_triple)?
      .ok_or_else(|| invalid("component selection disappeared"))
  }
}

/// Whether a bundle can run locally. A GNU Linux host can also run its portable
/// static musl target; a musl host cannot assume GNU dynamic libraries exist.
#[must_use]
pub fn local_target(target: &str) -> bool {
  let native = crate::paths::native_target();
  target == native
    || native
      .strip_suffix("-unknown-linux-gnu")
      .is_some_and(|arch| target == format!("{arch}-unknown-linux-musl"))
}

/// Resolves a component from the native local selection and checks its contracts.
/// No selection means legacy discovery can continue; a broken selection is an error.
///
/// # Errors
/// Rejects damaged selections, payloads or incompatible advertised contracts.
pub fn selected_executable(
  component: &str,
  required: &[(&str, &[ProtocolVersion])],
) -> io::Result<Option<PathBuf>> {
  let Some(home) = dirs::home_dir() else {
    return Ok(None);
  };
  selected_executable_at(&home, component, required)
}

/// Resolves a selected local component using an explicit account home.
///
/// # Errors
/// Rejects damaged selections, payloads or incompatible advertised contracts.
pub fn selected_executable_at(
  home: &Path,
  component: &str,
  required: &[(&str, &[ProtocolVersion])],
) -> io::Result<Option<PathBuf>> {
  let Some(bundle) = Store::new(home).selected(Purpose::Local, crate::paths::native_target())?
  else {
    return Ok(None);
  };
  let info = bundle
    .manifest
    .components
    .get(component)
    .ok_or_else(|| invalid("selected bundle omits the requested component"))?;
  if required.iter().any(|(name, supported)| {
    !info
      .protocols
      .iter()
      .any(|protocol| protocol.name == *name && protocol.negotiate(supported).is_some())
  }) {
    return Err(invalid(
      "selected bundle is incompatible; explicitly sync a compatible complete build",
    ));
  }
  if component == "ctld" && cfg!(target_os = "macos") {
    return Ok(Some(bundle.directory.join("ctld.app/Contents/MacOS/ctld")));
  }
  Ok(bundle.executable(component))
}

pub(super) fn digest(bytes: &[u8]) -> String {
  format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn valid_digest(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
pub(super) fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn validate_target(target: &str) -> io::Result<()> {
  if target.is_empty()
    || target.len() > 128
    || !target
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
  {
    return Err(invalid("invalid component target"));
  }
  if target != crate::paths::native_target()
    && ![
      "x86_64-unknown-linux-musl",
      "aarch64-unknown-linux-musl",
      "x86_64-unknown-linux-gnu",
      "aarch64-unknown-linux-gnu",
      "x86_64-apple-darwin",
      "aarch64-apple-darwin",
    ]
    .contains(&target)
  {
    return Err(invalid("unsupported component bundle target"));
  }
  Ok(())
}

fn safe_file(name: &str) -> bool {
  (COMPONENTS.contains(&name)
    || name == "manifest.json"
    || name == "ctld-package.json"
    || name.starts_with("ctld.app/Contents/"))
    && name.len() <= 512
    && name
      .split('/')
      .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', '\0']))
}
