//! Optional persistent storage for bundles verified by either download path.

use std::future::Future;
use std::path::PathBuf;

use ctl_client::remote_bundle::{self, BundleCacheEntry, VerifiedBundle};
use ctl_core::component::ComponentBuildInfo;

use super::super::Error;

pub(super) async fn get_or_download<F, FF>(
  root: Option<PathBuf>,
  target: &str,
  expected: &ComponentBuildInfo,
  download: F,
) -> Result<VerifiedBundle, Error>
where
  F: FnOnce() -> FF,
  FF: Future<Output = Result<VerifiedBundle, Error>>,
{
  let cache_root = if let Some(root) = root {
    let owned_target = target.to_owned();
    let expected = expected.clone();
    let owned_root = root.clone();
    let loaded = tokio::task::spawn_blocking(move || {
      remote_bundle::read_compatible_cached_bundle(&owned_root, &owned_target, &expected)
    })
    .await?;
    match loaded {
      Ok(Some(bundle)) => {
        eprintln!(
          "ctl: Using cached remote components {} ({}) for {target}.",
          crate::table::text(&bundle.app_version),
          &bundle.git_revision[..12],
        );
        return Ok(bundle);
      }
      Ok(None) => Some(root),
      Err(error) => {
        warn(&error);
        None
      }
    }
  } else {
    None
  };
  let bundle = download().await?;
  if let Some(root) = cache_root {
    let owned_target = target.to_owned();
    let expected = expected.clone();
    // Keep both the bundle and staging cleanup in the blocking worker, so an
    // interrupted caller cannot remove files while publication is in progress.
    let (bundle, stored) = tokio::task::spawn_blocking(move || {
      let result = BundleCacheEntry::new(&root, &owned_target, &expected)
        .and_then(|entry| entry.store(&bundle));
      (bundle, result)
    })
    .await?;
    if let Err(error) = stored {
      warn(&error);
    }
    return Ok(bundle);
  }
  Ok(bundle)
}

fn warn(error: &impl std::fmt::Display) {
  eprintln!(
    "ctl: Could not use the remote bundle cache: {}",
    crate::table::text(&error.to_string())
  );
}

#[cfg(test)]
#[path = "cache/tests.rs"]
mod tests;
