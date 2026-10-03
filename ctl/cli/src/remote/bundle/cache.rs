//! Optional persistent storage for bundles verified by either download path.

use std::future::Future;
use std::path::PathBuf;

use ctl_client::remote_bundle::{BundleCacheEntry, VerifiedBundle};
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
  let entry = if let Some(root) = root {
    let owned_target = target.to_owned();
    let expected = expected.clone();
    let loaded = tokio::task::spawn_blocking(move || {
      let entry = BundleCacheEntry::new(&root, &owned_target, &expected)?;
      let bundle = entry.load()?;
      Ok::<_, ctl_client::remote_bundle::Error>((entry, bundle))
    })
    .await?;
    match loaded {
      Ok((_, Some(bundle))) => {
        eprintln!("ctl: Using cached remote components for {target}.");
        return Ok(bundle);
      }
      Ok((entry, None)) => Some(entry),
      Err(error) => {
        warn(&error);
        None
      }
    }
  } else {
    None
  };
  let bundle = download().await?;
  if let Some(entry) = entry {
    // Keep both the bundle and staging cleanup in the blocking worker, so an
    // interrupted caller cannot remove files while publication is in progress.
    let (bundle, stored) = tokio::task::spawn_blocking(move || {
      let result = entry.store(&bundle);
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
