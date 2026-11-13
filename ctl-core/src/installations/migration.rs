//! Explicit adoption separates old discovery from the authoritative catalog.

use super::{CatalogEntry, CatalogOrigin, Installations, catalog, filesystem};
use crate::bundles::{Bundle, Manifest, Source};
use crate::private_fs::options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read as _};

const MARKER: &str = "catalog-migrated.json";
const MAX_MARKER_BYTES: u64 = 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Marker {
  schema_version: u16,
}

pub(super) fn is_migrated(store: &Installations) -> io::Result<bool> {
  if !filesystem::root_exists(store)? {
    return Ok(false);
  }
  let path = store.root.join(MARKER);
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
      "catalog migration marker exceeds its size limit",
    ));
  }
  let marker: Marker = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  if marker.schema_version != 1 {
    return Err(filesystem::invalid("unsupported catalog migration marker"));
  }
  if !catalog::exists(store)? {
    return Err(filesystem::invalid(
      "migrated installation catalog is missing",
    ));
  }
  Ok(true)
}

pub(super) fn migrate(store: &Installations) -> io::Result<Vec<CatalogEntry>> {
  filesystem::ensure_root(store)?;
  let _lock = filesystem::Lock::acquire(store)?;
  catalog::recover(store)?;
  if is_migrated(store)? {
    let entries = catalog::read(store)?.entries;
    validate_recorded_selection(store, &entries)?;
    return Ok(entries);
  }
  let mut state = catalog::read(store)?;
  let active = store.active()?;
  let previous = store.previous()?;
  reject_incomplete_selection(store, active.as_ref(), previous.as_ref())?;
  let mut bundles = store
    .list()?
    .into_iter()
    .map(|bundle| (bundle.manifest.bundle_id.clone(), bundle))
    .collect::<BTreeMap<_, _>>();
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
  Ok(state.entries)
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
        "legacy native migration requires the complete signed helper package; run ctlup update first",
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

fn validate_recorded_selection(store: &Installations, entries: &[CatalogEntry]) -> io::Result<()> {
  let active = store.active()?;
  let previous = store.previous()?;
  if active.is_none() && previous.is_some() {
    return Err(filesystem::invalid(
      "rollback installation has no active build",
    ));
  }
  for bundle in [active, previous].into_iter().flatten() {
    if !entries.iter().any(|entry| {
      entry.bundle_id == bundle.manifest.bundle_id
        && entry.target_triple == bundle.manifest.target_triple
    }) {
      return Err(filesystem::invalid(
        "uncatalogued selection; install a complete build with ctlup update",
      ));
    }
  }
  Ok(())
}

fn upgrade_required() -> io::Error {
  filesystem::invalid(
    "catalog migration requires complete schema-2 builds including ctl and ctmux; run ctlup update before migrating service-only or unmanaged installations",
  )
}

#[cfg(test)]
mod tests;
