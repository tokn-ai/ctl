//! Target-side installation of a publication selected by an authenticated SSH client.

use super::{BundleCacheEntry, BundleSet, MAX_BUNDLE_BYTES, host_download, published};
use ctl_core::bundles::{Manifest, Source};
use ctl_core::installations::{CatalogOrigin, Installations};
use std::io;
use std::path::Path;

/// Verifies and activates the pinned publication on the SSH host. A missing
/// download is the only recoverable outcome and precedes any installation.
///
/// # Errors
/// Reports invalid requests, integrity failures, unsafe storage or failed activation.
pub async fn install_host_publication(
  home: &Path,
  target: &str,
  archive_url: &str,
  origin: CatalogOrigin,
  manifest: Vec<u8>,
  supplied_archive: Option<Vec<u8>>,
) -> io::Result<Option<(Manifest, bool)>> {
  let set = BundleSet::parse_intrinsic(&manifest).map_err(io::Error::other)?;
  set
    .require_remote_installation(target)
    .map_err(io::Error::other)?;
  let url = validate_request(&set, target, archive_url, &origin)?;
  let build = &set
    .target(target)
    .map_err(io::Error::other)?
    .components
    .as_ref()
    .and_then(|components| components.get("ctl-agent"))
    .ok_or_else(|| io::Error::other("publication lacks ctl-agent metadata"))?
    .build;
  let account = ctl_core::paths::updater_directory_for_account(home);
  let cache =
    BundleCacheEntry::publication(&account.join("cache/downloads"), target, build, &manifest)
      .map_err(io::Error::other)?;
  let cached = if supplied_archive.is_none() {
    let entry = cache.clone();
    tokio::task::spawn_blocking(move || entry.load())
      .await
      .map_err(io::Error::other)?
      .map_err(io::Error::other)?
  } else {
    None
  };
  let verified = if let Some(cached) = cached {
    if cached.manifest != manifest {
      return Err(io::Error::other(
        "cached runtime differs from the pinned publication",
      ));
    }
    cached
  } else {
    let bytes = if let Some(bytes) = supplied_archive {
      bytes
    } else if let Some(bytes) = host_download::download(url.as_str(), MAX_BUNDLE_BYTES).await? {
      bytes
    } else {
      return Ok(None);
    };
    let target = target.to_owned();
    let built_at = origin.built_at.clone();
    tokio::task::spawn_blocking(move || {
      let mut verified = super::verify_installation_archive(manifest, &target, bytes)?;
      verified.built_at = built_at;
      cache.store(&verified)?;
      Ok::<_, super::Error>(verified)
    })
    .await
    .map_err(io::Error::other)?
    .map_err(io::Error::other)?
  };
  let source = if verified.app_version == verified.bundle_id {
    Source::Release
  } else {
    Source::Ci
  };
  let target = target.to_owned();
  let home = home.to_owned();
  let store_home = home.clone();
  let (bundle, reused) = tokio::task::spawn_blocking(move || {
    let bundle = crate::components::import_remote(&store_home, &verified, &target, source)
      .map_err(io::Error::other)?;
    let installations = Installations::new(&store_home);
    installations.prepare_installation()?;
    match installations.get_available(&bundle.manifest.bundle_id)? {
      Some(existing) if existing.manifest == bundle.manifest => Ok((existing, true)),
      Some(_) => Err(io::Error::other(
        "installed build differs from the verified publication",
      )),
      None => Ok((bundle, false)),
    }
  })
  .await
  .map_err(io::Error::other)??;
  crate::installation::provision_manager_from_for_build(
    &home,
    &std::env::current_exe()?,
    Some(&origin),
    &bundle.manifest.components["ctl-agent"].build,
  )
  .await?;
  let activated = tokio::task::spawn_blocking(move || {
    let (installed, _lease) =
      crate::installation::activate_uploaded(&home, &bundle, Some(&origin))?;
    if let Err(error) = Installations::new(&home).prune_unused() {
      eprintln!("ctlup: activated successfully; cleanup deferred: {error}");
    }
    if let Err(error) = crate::installation::prune_caches(&home) {
      eprintln!("ctlup: cache cleanup deferred: {error}");
    }
    Ok::<_, io::Error>(installed.manifest)
  })
  .await
  .map_err(io::Error::other)??;
  Ok(Some((activated, reused)))
}

fn validate_request(
  set: &BundleSet,
  target: &str,
  archive: &str,
  origin: &CatalogOrigin,
) -> io::Result<reqwest::Url> {
  origin.validate()?;
  let root = reqwest::Url::parse(published::ROOT).expect("valid publication root");
  let url = reqwest::Url::parse(archive).map_err(io::Error::other)?;
  let mut segments = url
    .path_segments()
    .ok_or_else(|| io::Error::other("invalid publication URL"))?
    .rev();
  if !ctl_core::bundles::local_target(target)
    || !published::trusted(&url, &root)
    || segments.next() != Some(set.archive_name(target).map_err(io::Error::other)?)
    || segments.next() != origin.publication_id.as_deref()
    || !matches!(origin.channel.as_str(), "stable" | "nightly" | "release")
    || (origin.channel == "stable"
      && (set.bundle_id != set.app_version
        || set
          .app_version
          .split('+')
          .next()
          .unwrap_or_default()
          .contains('-')))
  {
    return Err(io::Error::other(
      "archive URL, target or channel differs from the pinned publication",
    ));
  }
  Ok(url)
}
