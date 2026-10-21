//! Remote repair shares explicit update source policy, without sticky upload pins.

use super::Error;
#[cfg(not(unix))]
use ctl_client::remote_bundle;
use ctl_client::remote_bundle::VerifiedBundle;
use std::io;
use std::path::PathBuf;

pub(super) async fn matching_bundle(target: &str) -> Result<VerifiedBundle, Error> {
  let home = dirs::home_dir().ok_or_else(|| io::Error::other("home directory is unavailable"))?;
  #[cfg(unix)]
  {
    let source = crate::components::source_with_bundle_override(
      ctl_client::component_update::BuildSource::Automatic,
      std::env::var_os("CTL_REMOTE_BUNDLES_DIR").map(PathBuf::from),
    );
    let bundle = ctl_client::component_update::prepare(
      &home,
      target,
      ctl_core::bundles::Purpose::Upload,
      &source,
      &[],
    )
    .await?;
    eprintln!(
      "ctl: Prepared bundle {} (revision {})",
      bundle.manifest.bundle_id,
      bundle.manifest.components["ctl-agent"]
        .build
        .source_revision
        .as_deref()
        .unwrap_or("unknown")
    );
    Ok(tokio::task::spawn_blocking(move || ctl_client::components::upload_bundle(&bundle)).await??)
  }
  #[cfg(not(unix))]
  {
    if let Some(path) = std::env::var_os("CTL_REMOTE_BUNDLES_DIR") {
      let target = target.to_owned();
      return tokio::task::spawn_blocking(move || {
        remote_bundle::read_compatible_bundle(&[PathBuf::from(path)], &target).and_then(|bundle| {
          bundle.ok_or_else(|| {
            remote_bundle::Error::NotAvailable(
              "provided directory lacks a compatible complete bundle".into(),
            )
          })
        })
      })
      .await?
      .map_err(Into::into);
    }
    Ok(
      remote_bundle::published::download_cached(
        remote_bundle::published::default_channel(),
        target,
        &home.join(".tokn/ctl/published-bundles"),
      )
      .await?,
    )
  }
}

#[cfg(unix)]
pub(super) fn bundle_directories() -> io::Result<Vec<PathBuf>> {
  if let Some(directory) = std::env::var_os("CTL_REMOTE_BUNDLES_DIR") {
    return Ok(vec![PathBuf::from(directory)]);
  }
  let mut directories = Vec::new();
  if cfg!(debug_assertions) {
    directories.push(
      PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/desktop/src-tauri/resources/agent-bundles"),
    );
  }
  if let Some(parent) = std::env::current_exe()?.parent() {
    directories.push(parent.join("resources/agent-bundles"));
  }
  if let Some(home) = dirs::home_dir() {
    directories.push(home.join(".tokn/ctl/agent-bundles"));
  }
  Ok(directories)
}
