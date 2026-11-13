//! Complete target cohorts are committed together after every source is verified.

use super::{
  BuildSource, BundleChannel, Package, PreparationSnapshot, PreparedUpdate, UpdateResult, invalid,
};
use ctl_core::bundles::Bundle;
use ctl_core::component::ComponentBuildInfo;
use ctl_core::installations::{CatalogEntry, CatalogOrigin, Installations};
use std::collections::BTreeSet;
use std::io;
use std::path::Path;

/// Installs a channel's requested and previously registered target components
/// from one source publication, optionally selecting its native runtime.
///
/// # Errors
/// Rejects mixed publications, missing target artifacts, unsafe signing and
/// concurrent channel changes before changing the channel's installed heads.
pub async fn install_prepared_components(
  home: &Path,
  mut prepared: Vec<PreparedUpdate>,
  origin_override: Option<CatalogOrigin>,
  activate_native: bool,
) -> io::Result<Vec<UpdateResult>> {
  if prepared.is_empty() {
    return Err(invalid("choose at least one target component"));
  }
  let store = Installations::new(home);
  let snapshot = prepared
    .iter()
    .min_by_key(|component| component.snapshot.started)
    .ok_or_else(|| invalid("choose at least one target component"))?
    .snapshot
    .clone();
  let inventory = &snapshot.entries;
  for component in &mut prepared {
    if component.origin.is_none() {
      component.origin = Some(
        known_origin(inventory, &component.bundle)
          .unwrap_or_else(|| unknown_origin(&component.bundle)),
      );
    }
  }
  let first = &prepared[0];
  let actual_origin = first.origin.clone();
  let origin = effective_origin(&first.bundle, actual_origin.as_ref(), origin_override)?;
  let expected_heads = channel_heads(inventory, &origin.channel);
  check_preparation_heads(&store, &origin.channel, &expected_heads)?;
  let source_build = first.bundle.manifest.components["ctl-agent"].build.clone();
  let source = first.source.clone();
  let mut targets = BTreeSet::new();
  for component in &prepared {
    verify_component(component, &source_build, actual_origin.as_ref())?;
    if !targets.insert(component.bundle.manifest.target_triple.clone()) {
      return Err(invalid(
        "the requested components contain duplicate targets",
      ));
    }
  }

  // Snapshot heads before any supplemental network request. The core compares
  // this complete snapshot again under its installation lock before publishing.
  for head in &expected_heads {
    if targets.contains(&head.target_triple) {
      continue;
    }
    let existing = store.get(&head.bundle_id)?;
    let component = if head
      .origin
      .as_ref()
      .and_then(|origin| origin.publication_id.as_ref())
      == origin.publication_id.as_ref()
      && existing.manifest.components["ctl-agent"].build == source_build
    {
      retained_component(home, existing, head.origin.clone(), source.clone()).await?
    } else {
      prepare_sibling(home, &head.target_triple, &origin.channel, &source).await?
    };
    verify_component(&component, &source_build, actual_origin.as_ref())?;
    targets.insert(component.bundle.manifest.target_triple.clone());
    prepared.push(component);
  }

  verify_updaters(&prepared)?;
  provision_native(home, &prepared, activate_native).await?;
  let bundles = prepared
    .iter()
    .map(|component| component.bundle.clone())
    .collect::<Vec<_>>();
  let account = home.to_owned();
  let installed = tokio::task::spawn_blocking(move || {
    Installations::new(&account).install_channel_if_unchanged(
      &bundles,
      &origin,
      activate_native,
      &expected_heads,
    )
  })
  .await
  .map_err(io::Error::other)??;

  // Installation references are durable before republishing updater records.
  // Cache GC holds the installation lock while collecting its protected set.
  for component in prepared {
    if let (Some(info), Some(bytes)) = (component.updater_info, component.updater) {
      let saved = (|| {
        let updater = info.verify(bytes).map_err(io::Error::other)?;
        crate::installation::remember_verified_updater(home, &component.bundle.manifest, &updater)
      })();
      saved.map_err(|error| io::Error::other(format!(
        "channel installation completed; matching updater cache for {} could not be saved: {error}",
        component.bundle.manifest.target_triple
      )))?;
    }
  }
  if let Err(error) = store.prune() {
    eprintln!("ctlup: installed successfully; cleanup deferred: {error}");
  }
  super::prune_after_update(home).await;
  Ok(
    installed
      .iter()
      .map(|bundle| super::result(bundle, Package::FullBundle))
      .collect(),
  )
}

fn check_preparation_heads(
  store: &Installations,
  channel: &str,
  expected: &[CatalogEntry],
) -> io::Result<()> {
  if channel_heads(&store.catalog()?, channel) != expected {
    return Err(io::Error::new(
      io::ErrorKind::WouldBlock,
      "the channel's installed targets changed while preparing the source; retry the update",
    ));
  }
  Ok(())
}

fn verify_updaters(prepared: &[PreparedUpdate]) -> io::Result<()> {
  for component in prepared {
    if !ctl_core::bundles::local_target(&component.bundle.manifest.target_triple)
      && (component.updater.is_none() || component.updater_info.is_none())
    {
      return Err(invalid(
        "foreign target components require their matching verified updater",
      ));
    }
    if let (Some(info), Some(bytes)) = (&component.updater_info, &component.updater) {
      // Retain authenticated metadata with the bytes even if cache GC evicts
      // their preparation entry before the installed catalog is committed.
      let updater = info.verify(bytes.clone()).map_err(io::Error::other)?;
      crate::remote_bundle::published::validate_updater_for_manifest(
        &component.bundle.manifest,
        &updater,
      )
      .map_err(io::Error::other)?;
    }
  }
  Ok(())
}

async fn provision_native(
  home: &Path,
  prepared: &[PreparedUpdate],
  activate: bool,
) -> io::Result<()> {
  let Some(index) = native_component(prepared, activate)? else {
    return Ok(());
  };
  let component = &prepared[index];
  crate::components::verify_local(home, &component.bundle).await?;
  if let Some(bytes) = &component.updater {
    crate::installation::provision_manager_bytes(home, bytes.clone()).await
  } else {
    crate::installation::provision_manager(home, &component.bundle).await
  }
}

fn effective_origin(
  bundle: &Bundle,
  actual: Option<&CatalogOrigin>,
  requested: Option<CatalogOrigin>,
) -> io::Result<CatalogOrigin> {
  let origin = match (actual, requested) {
    (Some(actual), Some(requested)) => {
      if actual.publication_id != requested.publication_id || actual.built_at != requested.built_at
      {
        return Err(invalid(
          "a channel override must preserve the verified publication and build date",
        ));
      }
      requested
    }
    (Some(actual), None) => actual.clone(),
    (None, Some(_)) => {
      return Err(invalid(
        "a channel override requires verified source provenance",
      ));
    }
    (None, None) => unknown_origin(bundle),
  };
  origin.validate()?;
  Ok(origin)
}

pub(super) fn unknown_origin(bundle: &Bundle) -> CatalogOrigin {
  CatalogOrigin {
    channel: "provided".into(),
    publication_id: Some(
      bundle
        .manifest
        .distribution_id
        .as_ref()
        .unwrap_or(&bundle.manifest.bundle_id)
        .clone(),
    ),
    built_at: None,
  }
}

pub(super) fn known_origin(inventory: &[CatalogEntry], bundle: &Bundle) -> Option<CatalogOrigin> {
  inventory
    .iter()
    .filter(|entry| entry.bundle_id == bundle.manifest.bundle_id)
    .max_by_key(|entry| entry.current)
    .and_then(|entry| entry.origin.clone())
}

pub(super) fn channel_heads(inventory: &[CatalogEntry], channel: &str) -> Vec<CatalogEntry> {
  inventory
    .iter()
    .filter(|entry| {
      entry.current
        && entry
          .origin
          .as_ref()
          .is_some_and(|origin| origin.channel == channel)
    })
    .cloned()
    .collect()
}

fn verify_component(
  component: &PreparedUpdate,
  source_build: &ComponentBuildInfo,
  actual_origin: Option<&CatalogOrigin>,
) -> io::Result<()> {
  if component.bundle.manifest.schema_version != 2
    || &component.bundle.manifest.components["ctl-agent"].build != source_build
    || !crate::components::compatible(&component.bundle.manifest.components)
    || component
      .origin
      .as_ref()
      .map(|origin| &origin.publication_id)
      != actual_origin.map(|origin| &origin.publication_id)
  {
    return Err(invalid(
      "all channel targets must come from the same verified source publication",
    ));
  }
  Ok(())
}

fn native_component(prepared: &[PreparedUpdate], activate: bool) -> io::Result<Option<usize>> {
  if !activate {
    return Ok(None);
  }
  if let Some(index) = prepared.iter().position(|component| {
    component.bundle.manifest.target_triple == ctl_core::paths::native_target()
  }) {
    return Ok(Some(index));
  }
  let compatible = prepared
    .iter()
    .enumerate()
    .filter_map(|(index, component)| {
      ctl_core::bundles::local_target(&component.bundle.manifest.target_triple).then_some(index)
    })
    .collect::<Vec<_>>();
  match compatible.as_slice() {
    [index] => Ok(Some(*index)),
    _ => Err(invalid(
      "choose one unambiguous native component to activate",
    )),
  }
}

async fn retained_component(
  home: &Path,
  bundle: Bundle,
  origin: Option<CatalogOrigin>,
  source: BuildSource,
) -> io::Result<PreparedUpdate> {
  let snapshot = PreparationSnapshot::capture(home)?;
  let updater = Some(crate::installation::updater_for_bundle(home, &bundle).await?);
  let updater_info = crate::installation::supplied_updater_info(home, &bundle.manifest)?;
  Ok(PreparedUpdate {
    bundle,
    origin,
    source,
    updater,
    updater_info,
    remote_archive: None,
    snapshot,
  })
}

async fn prepare_sibling(
  home: &Path,
  target: &str,
  channel: &str,
  source: &BuildSource,
) -> io::Result<PreparedUpdate> {
  let published = match channel {
    "stable" => Some(BundleChannel::Stable),
    "nightly" => Some(BundleChannel::Nightly),
    _ => None,
  };
  if let Some(channel) = published {
    return super::prepare_component(
      home,
      target,
      &BuildSource::Published { channel },
      &[],
      |_| {},
    )
    .await;
  }
  if let Some(number) = channel.strip_prefix("pr-") {
    let number = number.parse::<u64>().map_err(io::Error::other)?;
    let downloaded = crate::remote_bundle::pr::download_for_target(number, target)
      .await
      .map_err(io::Error::other)?;
    let source = BuildSource::Provided {
      path: downloaded.directory().to_owned(),
      local_build: false,
      ctld_package: None,
    };
    return super::prepare_component(home, target, &source, &[], |_| {}).await;
  }
  super::prepare_component(home, target, source, &[], |_| {}).await
}

#[cfg(all(test, unix))]
mod tests;
