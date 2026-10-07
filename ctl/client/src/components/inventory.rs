//! Passive inventory of stored and included complete builds, shared by app and CLI.

use ctl_core::bundles::{Manifest, Purpose, Source, Store};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct AvailableBundle {
  pub manifest: Manifest,
  pub availability: Availability,
  pub selected_local: bool,
  pub selected_upload: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Availability {
  Included,
  Stored,
  IncludedAndStored,
}

impl Availability {
  #[must_use]
  pub fn included(self) -> bool {
    matches!(self, Self::Included | Self::IncludedAndStored)
  }

  #[must_use]
  pub fn stored(self) -> bool {
    matches!(self, Self::Stored | Self::IncludedAndStored)
  }
}

#[derive(Debug, serde::Serialize)]
pub struct Selection {
  pub target_triple: String,
  pub selected_local: Option<String>,
  pub selected_upload: Option<String>,
}

#[derive(Debug, Default)]
pub struct Snapshot {
  pub bundles: Vec<AvailableBundle>,
  pub selections: Vec<Selection>,
  pub errors: Vec<String>,
}

/// Lists all supported targets unless a filter is supplied. Never imports,
/// selects, executes binaries, downloads files, or contacts saved hosts.
#[must_use]
pub fn snapshot(home: &Path, target: Option<&str>, directories: &[PathBuf]) -> Snapshot {
  scan(home, target, |target| {
    included_bundle(directories, target).map(|bundle| bundle.map(|(_, manifest)| manifest))
  })
}

/// Inventories stored builds alongside an application's included-build provider.
/// An inspection failure is retained alongside any successfully inspected builds.
#[must_use]
pub fn scan(
  home: &Path,
  target: Option<&str>,
  mut included: impl FnMut(&str) -> Result<Option<Manifest>, crate::remote_bundle::Error>,
) -> Snapshot {
  let targets: BTreeSet<_> = target.map_or_else(
    || {
      [
        ctl_core::paths::native_target(),
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
      ]
      .into_iter()
      .collect()
    },
    |target| BTreeSet::from([target]),
  );
  let store = Store::new(home);
  let mut snapshot = Snapshot::default();
  for target in targets {
    let mut selected = |purpose| match store.selected(purpose, target) {
      Ok(bundle) => bundle.map(|bundle| bundle.manifest.bundle_id),
      Err(error) => {
        snapshot
          .errors
          .push(format!("{target} {purpose:?} selection: {error}"));
        None
      }
    };
    let local = selected(Purpose::Local);
    let upload = selected(Purpose::Upload);
    let entry = |manifest: Manifest, availability| AvailableBundle {
      selected_local: local.as_deref() == Some(&manifest.bundle_id),
      selected_upload: upload.as_deref() == Some(&manifest.bundle_id),
      manifest,
      availability,
    };
    match store.list(target) {
      Ok(bundles) => snapshot.bundles.extend(
        bundles
          .into_iter()
          .map(|bundle| entry(bundle.manifest, Availability::Stored)),
      ),
      Err(error) => snapshot.errors.push(format!("{target}: {error}")),
    }
    match included(target) {
      Ok(Some(manifest)) => {
        if let Some(stored) = snapshot.bundles.iter_mut().find(|bundle| {
          bundle.manifest.target_triple == target && bundle.manifest.bundle_id == manifest.bundle_id
        }) {
          stored.availability = Availability::IncludedAndStored;
        } else {
          snapshot
            .bundles
            .push(entry(manifest, Availability::Included));
        }
      }
      Ok(None) => {}
      Err(error) => snapshot
        .errors
        .push(format!("Included {target} bundle: {error}")),
    }
    snapshot.selections.push(Selection {
      target_triple: target.into(),
      selected_local: local,
      selected_upload: upload,
    });
  }
  snapshot
}

fn included_bundle(
  directories: &[PathBuf],
  target: &str,
) -> Result<Option<(crate::remote_bundle::VerifiedBundle, Manifest)>, crate::remote_bundle::Error> {
  if !super::upload_target(target) {
    return Ok(None);
  }
  let Some(bundle) = crate::remote_bundle::read_compatible_bundle(directories, target)? else {
    return Ok(None);
  };
  let source = if bundle.bundle_id == bundle.app_version {
    Source::Release
  } else {
    Source::Ci
  };
  let manifest = super::inspect_remote(&bundle, target, source)?;
  Ok(Some((bundle, manifest)))
}

/// Loads a stored build or imports the exact included build explicitly chosen.
///
/// # Errors
/// Rejects unsafe identities, corrupt stored data, and changed included builds.
pub fn load_selection(
  home: &Path,
  directories: &[PathBuf],
  target: &str,
  id: &str,
) -> io::Result<ctl_core::bundles::Bundle> {
  match Store::new(home).get(target, id) {
    Ok(bundle) => return Ok(bundle),
    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
    Err(error) => return Err(error),
  }
  let (archive, manifest) = included_bundle(directories, target)
    .map_err(io::Error::other)?
    .filter(|(_, manifest)| manifest.bundle_id == id)
    .ok_or_else(|| {
      io::Error::other(
        "This included build changed or is unavailable. Refresh bundles and choose again.",
      )
    })?;
  super::import_remote(home, &archive, target, manifest.source).map_err(io::Error::other)
}

#[cfg(test)]
mod tests;
