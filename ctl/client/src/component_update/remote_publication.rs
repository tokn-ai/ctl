use super::{
  BundleChannel, PreparationProgress, PreparedUpdate, prepare_published_upload_with_cached,
};
use crate::{
  PublishedInstallStage, SshConnectionOptions, SshInteraction, remote_bundle::published,
};
use ctl_core::bundles::Manifest;
use std::{io, path::Path};

pub struct PublicationTarget<'a> {
  pub destination: &'a str,
  pub options: &'a SshConnectionOptions,
  pub interaction: &'a SshInteraction,
  pub expected_account: Option<&'a str>,
  pub target_triple: &'a str,
}

pub enum PublicationDeployment {
  Installed(Manifest),
  Upload(Box<PreparedUpdate>),
}

/// Checks the verified local cache for each artifact, then tries an on-host
/// fetch for missing bytes. Only explicit pre-install unavailability falls back.
///
/// # Errors
/// Reports publication, SSH, integrity and activation errors without retrying mutations.
pub async fn deploy_remote_publication(
  home: &Path,
  target: &PublicationTarget<'_>,
  channel: BundleChannel,
  on_stage: impl Fn(PublishedInstallStage) + Send + Sync,
  on_prepare: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<PublicationDeployment> {
  on_stage(PublishedInstallStage::CheckingLocalCache);
  let plan = published::resolve_installation(channel, target.target_triple)
    .await
    .map_err(io::Error::other)?;
  deploy(home, target, &plan, on_stage, on_prepare).await
}

/// Deploys the exact retained publication, recording a pin instead of a tracked channel.
///
/// # Errors
/// Reports invalid publications, SSH, integrity and activation failures.
pub async fn deploy_remote_release(
  home: &Path,
  target: &PublicationTarget<'_>,
  publication_id: &str,
  on_stage: impl Fn(PublishedInstallStage) + Send + Sync,
  on_prepare: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<PublicationDeployment> {
  on_stage(PublishedInstallStage::CheckingLocalCache);
  let plan = published::resolve_release_installation(publication_id, target.target_triple, true)
    .await
    .map_err(io::Error::other)?;
  deploy(home, target, &plan, on_stage, on_prepare).await
}

async fn deploy(
  home: &Path,
  target: &PublicationTarget<'_>,
  plan: &published::InstallationPlan,
  on_stage: impl Fn(PublishedInstallStage) + Send + Sync,
  on_prepare: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<PublicationDeployment> {
  let cache = ctl_core::paths::updater_directory_for_account(home).join("cache");
  let updater_root = cache.join("updaters");
  let runtime_root = cache.join("downloads");
  let (updater, runtime) = tokio::try_join!(
    plan.cached_updater(&updater_root),
    plan.cached_runtime(&runtime_root)
  )
  .map_err(io::Error::other)?;
  if updater.is_some() && runtime.is_some() {
    on_stage(PublishedInstallStage::UsingLocalCache);
  } else {
    let installed = crate::try_install_published_remote(
      target.destination,
      target.options,
      target.interaction,
      target.expected_account,
      plan,
      updater.as_ref().map(|file| file.bytes.as_slice()),
      runtime.as_ref().map(|file| file.archive.as_slice()),
      &on_stage,
    )
    .await
    .map_err(io::Error::other)?;
    if let Some(manifest) = installed {
      return Ok(PublicationDeployment::Installed(manifest));
    }
    on_stage(PublishedInstallStage::UploadFallback);
  }
  let prepared =
    prepare_published_upload_with_cached(home, plan, runtime, updater, on_prepare).await?;
  Ok(PublicationDeployment::Upload(Box::new(prepared)))
}
