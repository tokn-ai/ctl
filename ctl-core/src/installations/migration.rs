//! One-time adoption separates old discovery from the authoritative catalog.

use super::{CatalogEntry, CatalogOrigin, Installations, catalog, filesystem};
use crate::bundles::{Bundle, Manifest, Source};
use crate::private_fs::options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read as _};

const MARKER: &str = "catalog-migrated.json";
const LAYOUT_MARKER: &str = "install-layout-migrated.json";
const MAX_MARKER_BYTES: u64 = 1024;

mod legacy;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Marker {
  schema_version: u16,
}

fn read_marker(store: &Installations, name: &str) -> io::Result<bool> {
  let path = store.root.join(name);
  match fs::symlink_metadata(&path) {
    Ok(metadata) => filesystem::private_file(&metadata)?,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(error),
  }
  let file = options().read(true).open(path)?;
  filesystem::private_file(&file.metadata()?)?;
  let mut bytes = Vec::new();
  file.take(MAX_MARKER_BYTES + 1).read_to_end(&mut bytes)?;
  if bytes.len() as u64 > MAX_MARKER_BYTES {
    return Err(filesystem::invalid(
      "installation migration marker exceeds its size limit",
    ));
  }
  let marker: Marker = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  if marker.schema_version != 1 {
    return Err(filesystem::invalid(
      "unsupported installation migration marker",
    ));
  }
  if !catalog::exists(store)? {
    return Err(filesystem::invalid(
      "migrated installation catalog is missing",
    ));
  }
  Ok(true)
}

pub(super) fn migrate_once_if_needed(store: &Installations) -> io::Result<()> {
  if !filesystem::root_exists(store)?
    || read_marker(store, LAYOUT_MARKER)?
    || !has_migratable_data(store)?
  {
    return Ok(());
  }
  let _lock = filesystem::Lock::acquire(store)?;
  if read_marker(store, LAYOUT_MARKER)? || !has_migratable_data(store)? {
    return Ok(());
  }
  let state =
    if read_marker(store, MARKER)? || catalog::exists(store)? || catalog::has_transaction(store)? {
      recover_catalog(store)?
    } else {
      adopt_legacy(store)?
    };
  filesystem::ensure_directory(&store.root.join("installs"))?;
  if let Err(error) = super::layout::sync(store, &state) {
    eprintln!("ctlup: installation paths could not be refreshed: {error}");
    return Ok(());
  }
  catalog::replace(
    &store.root.join(LAYOUT_MARKER),
    &Marker { schema_version: 1 },
  )
}

fn has_migratable_data(store: &Installations) -> io::Result<bool> {
  Ok(
    filesystem::path_exists(&store.root.join(MARKER))?
      || catalog::exists(store)?
      || catalog::has_transaction(store)?
      || catalog::has_installation_data(store)?,
  )
}

fn recover_catalog(store: &Installations) -> io::Result<catalog::State> {
  #[cfg(unix)]
  legacy::require_recoverable_activation(store)?;
  catalog::recover(store)?;
  let state = catalog::read_projection(store)?;
  let active = store.recorded_selection("current")?;
  let previous = store.recorded_selection("previous")?;
  if active.is_none() && previous.is_some() {
    return Err(filesystem::invalid(
      "previous installation has no active build",
    ));
  }
  Ok(state)
}

fn adopt_legacy(store: &Installations) -> io::Result<catalog::State> {
  let mut bundles = legacy::managed_bundles(store)?
    .into_iter()
    .map(|bundle| (bundle.manifest.bundle_id.clone(), bundle))
    .collect::<BTreeMap<_, _>>();
  if bundles
    .values()
    .any(|bundle| bundle.manifest.schema_version != 2)
  {
    return Err(upgrade_required());
  }
  #[cfg(unix)]
  legacy::require_recoverable_activation(store)?;
  catalog::recover(store)?;
  let mut state = catalog::read_for_migration(store)?;
  let active = legacy::active(store)?;
  let previous = legacy::previous(store)?;
  reject_incomplete_selection(store, active.as_ref(), previous.as_ref())?;
  for bundle in [active.as_ref(), previous.as_ref()].into_iter().flatten() {
    bundles
      .entry(bundle.manifest.bundle_id.clone())
      .or_insert_with(|| bundle.clone());
  }
  // Validate every candidate before publishing any copied legacy payload or
  // committing metadata. Service-only manifests cannot become complete builds.
  for bundle in bundles.values() {
    if bundle.manifest.schema_version != 2 {
      return Err(upgrade_required());
    }
    filesystem::validate_complete(bundle)?;
    drop(bundle.read_files()?);
  }
  filesystem::ensure_directory(&store.root.join("installs"))?;
  let (active, previous, canonicalize) = import_selection(store, active, previous)?;
  for bundle in bundles.values() {
    if state
      .entries
      .iter()
      .any(|entry| entry.bundle_id == bundle.manifest.bundle_id)
    {
      continue;
    }
    let origin = adopted_origin(&bundle.manifest)?;
    let current = active
      .as_ref()
      .is_some_and(|active| active.manifest.bundle_id == bundle.manifest.bundle_id)
      && !state.entries.iter().any(|entry| {
        entry.current
          && entry
            .origin
            .as_ref()
            .is_some_and(|existing| existing.channel == origin.channel)
      });
    state.entries.push(CatalogEntry {
      bundle_id: bundle.manifest.bundle_id.clone(),
      target_triple: bundle.manifest.target_triple.clone(),
      origin: Some(origin),
      current,
    });
  }
  let activation = if canonicalize {
    active.as_ref().map(|active| (active, previous.as_ref()))
  } else {
    None
  };
  catalog::commit(store, &state, activation)?;
  // Both catalog and canonical selection are durable before this cutover. A
  // crash before the marker simply repeats an idempotent verified adoption.
  catalog::replace(&store.root.join(MARKER), &Marker { schema_version: 1 })?;
  Ok(state)
}

fn adopted_origin(manifest: &Manifest) -> io::Result<CatalogOrigin> {
  let local = manifest.source == Source::Local;
  let publication_id = if local {
    format!(
      "local.{}",
      manifest.components["ctl-agent"].build.source_fingerprint
    )
  } else {
    manifest
      .distribution_id
      .as_ref()
      .unwrap_or(&manifest.bundle_id)
      .clone()
  };
  let origin = CatalogOrigin {
    channel: if local { "local" } else { "provided" }.into(),
    publication_id: Some(publication_id),
    built_at: None,
  };
  origin.validate()?;
  Ok(origin)
}

fn reject_incomplete_selection(
  store: &Installations,
  active: Option<&Bundle>,
  previous: Option<&Bundle>,
) -> io::Result<()> {
  #[cfg(unix)]
  if active.is_none() && filesystem::path_exists(&store.root.join("current"))? {
    return Err(upgrade_required());
  }
  #[cfg(windows)]
  let _ = store;
  if active.is_none() && previous.is_some() {
    return Err(filesystem::invalid(
      "rollback installation has no active build",
    ));
  }
  Ok(())
}

type ImportedSelection = (Option<Bundle>, Option<Bundle>, bool);

fn import_selection(
  store: &Installations,
  active: Option<Bundle>,
  previous: Option<Bundle>,
) -> io::Result<ImportedSelection> {
  for bundle in [active.as_ref(), previous.as_ref()].into_iter().flatten() {
    let destination = store.root.join("installs").join(&bundle.manifest.bundle_id);
    if bundle.directory == destination {
      continue;
    }
    // Preserve the complete package when moving a trusted native selection;
    // a flat remote broker cannot stand in for the signed macOS helper.
    if cfg!(target_os = "macos")
      && (!bundle
        .manifest
        .files
        .contains_key("ctld.app/Contents/MacOS/ctld")
        || !bundle.manifest.files.contains_key("ctld-package.json"))
    {
      return Err(filesystem::invalid(
        "native migration requires the complete signed helper package; replace the installation using the old manager before migrating, or perform a clean reinstall",
      ));
    }
  }
  let mut canonicalize = false;
  let mut import = |bundle: Bundle| {
    let destination = store.root.join("installs").join(&bundle.manifest.bundle_id);
    if bundle.directory == destination {
      return Ok(bundle);
    }
    canonicalize = true;
    filesystem::publish(store, &bundle, None)
  };
  let active = active.map(&mut import).transpose()?;
  let previous = previous.map(&mut import).transpose()?;
  Ok((active, previous, canonicalize))
}

fn upgrade_required() -> io::Error {
  filesystem::invalid(
    "catalog migration requires complete schema-2 builds including ctl and ctmux; replace service-only or unmanaged installations using the old manager before migrating, or perform a clean reinstall",
  )
}

#[cfg(test)]
mod tests;
