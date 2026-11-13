//! Compact build details are separate from the original status and catalog JSON.

use super::{InstallationCatalog, InstallationStatus};
use ctl_core::bundles::{Manifest, Source};
use ctl_core::component::ComponentBuildInfo;
use ctl_core::installations::{CatalogEntry, Installations};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

pub(super) const MAX_SHOW_BYTES: usize = 4 * 1024 * 1024;
const MAX_BUILDS: usize = 4096;

/// Verified source identity of one immutable installed build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledBuild {
  pub bundle_id: String,
  pub target_triple: String,
  pub source: Source,
  pub build: ComponentBuildInfo,
}

impl InstalledBuild {
  fn from_manifest(manifest: &Manifest) -> io::Result<Self> {
    manifest.validate()?;
    Ok(Self {
      bundle_id: manifest.bundle_id.clone(),
      target_triple: manifest.target_triple.clone(),
      source: manifest.source,
      build: manifest
        .components
        .get("ctl-agent")
        .ok_or_else(|| invalid("installed build has no agent identity"))?
        .build
        .clone(),
    })
  }

  fn validate(&self) -> io::Result<()> {
    CatalogEntry {
      bundle_id: self.bundle_id.clone(),
      target_triple: self.target_triple.clone(),
      origin: None,
      current: false,
    }
    .validate()?;
    if !self.build.is_valid()
      || self.build.source_revision.is_none()
      || (self.build.dirty && self.source != Source::Local)
    {
      return Err(invalid("invalid installed build identity"));
    }
    Ok(())
  }
}

/// Optional detailed inventory, independent of the original status response.
/// Older managers can provide catalog entries without their build summaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationShow {
  pub schema_version: u16,
  pub active: Option<String>,
  pub previous: Option<String>,
  pub installations: Vec<CatalogEntry>,
  pub builds: Vec<InstalledBuild>,
}

impl InstallationShow {
  /// Combines older status and optional catalog responses without inventing
  /// source details for builds that the older manager cannot describe.
  ///
  /// # Errors
  /// Reports invalid manifests, conflicting identities or malformed catalogs.
  pub fn from_status(
    status: InstallationStatus,
    catalog: Option<InstallationCatalog>,
  ) -> io::Result<Self> {
    let mut installations = match catalog {
      Some(catalog) => {
        catalog.validate()?;
        catalog.installations
      }
      None => Vec::new(),
    };
    let mut builds = BTreeMap::new();
    for manifest in [status.active.as_ref(), status.previous.as_ref()]
      .into_iter()
      .flatten()
    {
      let build = InstalledBuild::from_manifest(manifest)?;
      ensure_installation(&mut installations, &build);
      insert_build(&mut builds, build)?;
    }
    let result = Self {
      schema_version: 1,
      active: status.active.map(|manifest| manifest.bundle_id),
      previous: status.previous.map(|manifest| manifest.bundle_id),
      installations,
      builds: builds.into_values().collect(),
    };
    result.validate()?;
    Ok(result)
  }

  /// Validates bounded source metadata and the inventory's identity joins.
  /// Missing summaries are allowed only for unselected older catalog entries.
  ///
  /// # Errors
  /// Reports unsupported schemas, duplicate identities, conflicting targets,
  /// missing selected builds or an oversized response.
  pub fn validate(&self) -> io::Result<()> {
    if self.schema_version != 1 || self.builds.len() > MAX_BUILDS {
      return Err(invalid("invalid installation show schema or size"));
    }
    InstallationCatalog::new(self.installations.clone()).validate()?;
    let mut builds = BTreeMap::new();
    for build in &self.builds {
      build.validate()?;
      if builds.insert(build.bundle_id.as_str(), build).is_some() {
        return Err(invalid("duplicate installed build identity"));
      }
    }
    let mut targets = BTreeMap::new();
    for entry in &self.installations {
      if targets
        .insert(entry.bundle_id.as_str(), entry.target_triple.as_str())
        .is_some_and(|target| target != entry.target_triple)
        || builds
          .get(entry.bundle_id.as_str())
          .is_some_and(|build| build.target_triple != entry.target_triple)
      {
        return Err(invalid("installed build and catalog targets differ"));
      }
    }
    if builds.keys().any(|id| !targets.contains_key(id)) {
      return Err(invalid("installed build is absent from the catalog"));
    }
    if [self.active.as_deref(), self.previous.as_deref()]
      .into_iter()
      .flatten()
      .any(|id| !builds.contains_key(id))
    {
      return Err(invalid("selected installation has no build details"));
    }
    if self.active.is_some() && self.active == self.previous {
      return Err(invalid("active and previous installations must differ"));
    }
    // Reserve the newline emitted by the CLI within the JSON payload bound.
    if serde_json::to_vec(self).map_err(io::Error::other)?.len() >= MAX_SHOW_BYTES {
      return Err(invalid("installation show response exceeds its size limit"));
    }
    Ok(())
  }
}

/// Lists verified installed builds and native history without changing storage.
/// Legacy selections outside the managed payload directory remain visible.
///
/// # Errors
/// Reports unsafe storage, modified builds or a concurrently removed payload.
pub fn show(home: &Path) -> io::Result<InstallationShow> {
  let store = Installations::new(home);
  let mut result = InstallationShow::from_status(
    super::status(home)?,
    Some(InstallationCatalog::new(store.catalog()?)),
  )?;
  let mut builds = result
    .builds
    .into_iter()
    .map(|build| (build.bundle_id.clone(), build))
    .collect::<BTreeMap<_, _>>();
  for bundle in store.list()? {
    let build = InstalledBuild::from_manifest(&bundle.manifest)?;
    ensure_installation(&mut result.installations, &build);
    insert_build(&mut builds, build)?;
  }
  if result
    .installations
    .iter()
    .any(|entry| !builds.contains_key(&entry.bundle_id))
  {
    return Err(io::Error::new(
      io::ErrorKind::WouldBlock,
      "installed builds changed during inspection; retry the command",
    ));
  }
  result.builds = builds.into_values().collect();
  result.validate()?;
  Ok(result)
}

fn ensure_installation(installations: &mut Vec<CatalogEntry>, build: &InstalledBuild) {
  if !installations
    .iter()
    .any(|entry| entry.bundle_id == build.bundle_id)
  {
    installations.push(CatalogEntry {
      bundle_id: build.bundle_id.clone(),
      target_triple: build.target_triple.clone(),
      origin: None,
      current: false,
    });
  }
}

fn insert_build(
  builds: &mut BTreeMap<String, InstalledBuild>,
  build: InstalledBuild,
) -> io::Result<()> {
  if builds
    .get(&build.bundle_id)
    .is_some_and(|existing| *existing != build)
  {
    return Err(invalid("conflicting installed build identities"));
  }
  builds.insert(build.bundle_id.clone(), build);
  Ok(())
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::remote_bundle::compatibility::tests::Fixture;
  use ctl_core::bundles::Store;
  use ctl_core::installations::CatalogOrigin;
  use std::path::PathBuf;

  struct Home(PathBuf);
  impl Home {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ctlup-show-{}", uuid::Uuid::new_v4()));
      ctl_core::private_fs::create_directory(&path).unwrap();
      Self(path)
    }
  }
  impl Drop for Home {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn fixture(target: &str, revision: char) -> (Manifest, BTreeMap<String, Vec<u8>>) {
    let fixture = Fixture::new("0.1.3", &revision.to_string().repeat(40)).with_clients();
    let components = serde_json::from_value(fixture.inner["components"].clone()).unwrap();
    let manifest = Manifest::new(target, Source::Local, components, &fixture.files).unwrap();
    (manifest, fixture.files)
  }

  fn snapshot() -> InstallationShow {
    let (manifest, _) = fixture("x86_64-unknown-linux-musl", 'a');
    InstallationShow::from_status(
      InstallationStatus {
        active: Some(manifest),
        previous: None,
      },
      None,
    )
    .unwrap()
  }

  #[test]
  fn absent_inventory_is_passive() {
    let home = Home::new();
    let result = show(&home.0).unwrap();
    assert!(result.active.is_none());
    assert!(result.previous.is_none());
    assert!(result.installations.is_empty());
    assert!(result.builds.is_empty());
    assert!(!home.0.join(".tokn").exists());
  }

  #[test]
  fn fallback_preserves_known_source_and_unknown_extra_builds() {
    let (active, _) = fixture("x86_64-unknown-linux-musl", 'a');
    let (previous, _) = fixture("x86_64-unknown-linux-musl", 'b');
    let extra = CatalogEntry {
      bundle_id: "c".repeat(64),
      target_triple: "aarch64-unknown-linux-musl".into(),
      origin: Some(CatalogOrigin {
        channel: "nightly".into(),
        publication_id: Some("latest".into()),
        built_at: None,
      }),
      current: true,
    };
    let result = InstallationShow::from_status(
      InstallationStatus {
        active: Some(active.clone()),
        previous: Some(previous.clone()),
      },
      Some(InstallationCatalog::new(vec![extra.clone()])),
    )
    .unwrap();
    assert_eq!(result.active.as_ref(), Some(&active.bundle_id));
    assert_eq!(result.previous.as_ref(), Some(&previous.bundle_id));
    assert_eq!(result.builds.len(), 2);
    assert!(
      result
        .builds
        .iter()
        .all(|build| build.source == Source::Local)
    );
    assert!(result.installations.contains(&extra));
    assert_eq!(
      result
        .installations
        .iter()
        .filter(|entry| entry.origin.is_none())
        .count(),
      2
    );
    assert!(
      !result
        .builds
        .iter()
        .any(|build| build.bundle_id == extra.bundle_id)
    );
  }

  #[test]
  fn foreign_inventory_joins_one_build_to_multiple_channel_registrations() {
    let home = Home::new();
    let (manifest, files) = fixture("aarch64-unknown-linux-musl", 'a');
    let bundle = Store::cache(&home.0).publish(&manifest, &files).unwrap();
    let store = Installations::new(&home.0);
    for channel in ["nightly", "provided"] {
      store
        .install_channel(
          std::slice::from_ref(&bundle),
          &CatalogOrigin {
            channel: channel.into(),
            publication_id: Some("same-source".into()),
            built_at: Some("2026-01-02T03:04:05Z".into()),
          },
          false,
        )
        .unwrap();
    }
    let result = show(&home.0).unwrap();
    assert!(result.active.is_none());
    assert_eq!(result.builds.len(), 1);
    assert_eq!(result.builds[0].bundle_id, manifest.bundle_id);
    assert_eq!(
      result.builds[0].build,
      manifest.components["ctl-agent"].build
    );
    assert_eq!(result.installations.len(), 2);
    assert!(result.installations.iter().all(|entry| entry.current));
  }

  #[cfg(unix)]
  #[test]
  fn legacy_native_selection_is_visible_without_migrating_its_payload() {
    let home = Home::new();
    let (manifest, mut files) = fixture(ctl_core::paths::native_target(), 'a');
    if cfg!(target_os = "macos") {
      files.insert("ctld.app/Contents/MacOS/ctld".into(), files["ctld"].clone());
      files.insert("ctld-package.json".into(), b"receipt fixture".to_vec());
    }
    let manifest = Manifest::new(
      &manifest.target_triple,
      Source::Local,
      manifest.components,
      &files,
    )
    .unwrap();
    let legacy = Store::new(&home.0);
    let bundle = legacy.publish(&manifest, &files).unwrap();
    legacy
      .select(ctl_core::bundles::Purpose::Local, &bundle)
      .unwrap();
    let result = show(&home.0).unwrap();
    assert_eq!(result.active.as_ref(), Some(&manifest.bundle_id));
    assert_eq!(result.builds.len(), 1);
    assert_eq!(result.builds[0].source, Source::Local);
    assert_eq!(result.installations.len(), 1);
    assert!(result.installations[0].origin.is_none());
    assert!(!result.installations[0].current);
    assert!(!home.0.join(".tokn/ctl/installs").exists());
  }

  #[test]
  fn malformed_and_conflicting_inventory_joins_are_rejected() {
    let valid = snapshot();
    let mut invalid = valid.clone();
    invalid.builds.push(invalid.builds[0].clone());
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.installations[0].target_triple = "aarch64-unknown-linux-musl".into();
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.installations.clear();
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.builds.clear();
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.previous.clone_from(&invalid.active);
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.builds[0].build.version.push('\n');
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.schema_version = 2;
    assert!(invalid.validate().is_err());

    let mut mismatched = valid.installations[0].clone();
    mismatched.target_triple = "aarch64-unknown-linux-musl".into();
    mismatched.origin = Some(CatalogOrigin {
      channel: "provided".into(),
      publication_id: None,
      built_at: None,
    });
    invalid = valid;
    invalid.installations.push(mismatched);
    assert!(invalid.validate().is_err());
  }

  #[test]
  fn serialized_inventory_has_a_bounded_size() {
    let mut result = snapshot();
    let mut build = result.builds.pop().unwrap();
    build.build.version = "v".repeat(256);
    build.build.source_revision = Some("a".repeat(64));
    result.active = None;
    result.installations.clear();
    for index in 0..MAX_BUILDS {
      build.bundle_id = format!("{index:064x}");
      result.installations.push(CatalogEntry {
        bundle_id: build.bundle_id.clone(),
        target_triple: build.target_triple.clone(),
        origin: Some(CatalogOrigin {
          channel: "provided".into(),
          publication_id: Some("p".repeat(256)),
          built_at: Some("2026-01-02T03:04:05.123456789Z".into()),
        }),
        current: false,
      });
      result.builds.push(build.clone());
    }
    assert!(serde_json::to_vec(&result).unwrap().len() > MAX_SHOW_BYTES);
    assert!(
      result
        .validate()
        .unwrap_err()
        .to_string()
        .contains("size limit")
    );
  }
}
