//! Resolve an installation once so remote fetching and SSH fallback use identical assets.

use super::{
  BundleChannel, DownloadArtifact, DownloadProgress, Error, Publication, Resolver, UpdaterInfo,
  VerifiedBundle, VerifiedUpdater,
};
use ctl_core::component::ComponentInfo;
use ctl_core::installations::CatalogOrigin;
use std::collections::BTreeMap;
use std::path::Path;

/// Metadata from one immutable target publication. Payloads are fetched only when requested.
pub struct InstallationPlan {
  resolver: Resolver,
  publication: Publication,
  channel: BundleChannel,
  origin: CatalogOrigin,
  updater: UpdaterInfo,
  updater_url: reqwest::Url,
  archive_url: reqwest::Url,
  components: BTreeMap<String, ComponentInfo>,
}

/// Resolves a complete, compatible publication for an SSH target without downloading payloads.
///
/// # Errors
/// Returns unavailable, unsafe or mismatched publication metadata before remote mutation.
pub async fn resolve_installation(
  channel: BundleChannel,
  target: &str,
) -> Result<InstallationPlan, Error> {
  let plan = resolve_target_installation(channel, target).await?;
  plan
    .publication
    .manifest
    .require_remote_installation(target)?;
  Ok(plan)
}

pub(crate) async fn resolve_target_installation(
  channel: BundleChannel,
  target: &str,
) -> Result<InstallationPlan, Error> {
  Resolver::production()?
    .installation_plan(channel, target)
    .await
}

impl Resolver {
  pub(super) async fn installation_plan(
    &self,
    channel: BundleChannel,
    target: &str,
  ) -> Result<InstallationPlan, Error> {
    let publication = self.target_publication(channel, target).await?;
    let entry = publication.manifest.target(target)?;
    let components = Self::require_compatible(&publication, target)?.clone();
    let updater = self.updater_metadata(&publication, target).await?;
    let archive_url = publication
      .url
      .join(&entry.archive)
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let updater_url = publication
      .url
      .join(&updater.binary)
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let origin = CatalogOrigin {
      channel: channel.name().into(),
      publication_id: publication
        .id
        .clone()
        .or_else(|| Some(publication.manifest.bundle_id.clone())),
      built_at: publication.built_at.clone(),
    };
    origin.validate()?;
    Ok(InstallationPlan {
      resolver: self.clone(),
      publication,
      channel,
      origin,
      updater,
      updater_url,
      archive_url,
      components,
    })
  }
}

impl InstallationPlan {
  #[must_use]
  pub fn channel(&self) -> BundleChannel {
    self.channel
  }

  #[must_use]
  pub fn target(&self) -> &str {
    &self.updater.target_triple
  }

  #[must_use]
  pub fn origin(&self) -> &CatalogOrigin {
    &self.origin
  }

  #[must_use]
  pub fn manifest_bytes(&self) -> &[u8] {
    &self.publication.bytes
  }

  #[must_use]
  pub fn archive_url(&self) -> &str {
    self.archive_url.as_str()
  }

  #[must_use]
  pub fn updater_url(&self) -> &str {
    self.updater_url.as_str()
  }

  #[must_use]
  pub fn updater_info(&self) -> &UpdaterInfo {
    &self.updater
  }

  #[must_use]
  pub fn components(&self) -> &BTreeMap<String, ComponentInfo> {
    &self.components
  }

  /// Verifies a local updater against the pinned SHA-256 and size without downloading anything.
  ///
  /// # Errors
  /// Reports inaccessible or unsafe cache paths. Missing or damaged data returns `None`.
  pub async fn cached_updater(&self, cache_root: &Path) -> Result<Option<VerifiedUpdater>, Error> {
    super::updater::cached_updater(cache_root, &self.updater).await
  }

  /// Verifies the local archive against its pinned manifest and digest without a network request.
  ///
  /// # Errors
  /// Reports unsafe cache paths or a conflicting manifest. Missing or damaged data returns `None`.
  pub async fn cached_runtime(&self, cache_root: &Path) -> Result<Option<VerifiedBundle>, Error> {
    let entry = self.publication.cache_entry(self.target(), cache_root)?;
    self.publication.cached_archive(&entry).await
  }

  /// Retrieves just the pinned target updater, verifying its bytes before cache use.
  ///
  /// # Errors
  /// Reports unavailable or corrupt assets and unsafe cache storage.
  pub async fn download_updater(
    &self,
    cache_root: &Path,
    progress: impl Fn(DownloadProgress) + Send + Sync,
  ) -> Result<VerifiedUpdater, Error> {
    self
      .resolver
      .pinned_updater_cached(
        &self.publication,
        self.updater.clone(),
        cache_root,
        Some((DownloadArtifact::Updater, &progress)),
      )
      .await
  }

  /// Retrieves just the pinned runtime archive, verifying the original manifest and digests.
  ///
  /// # Errors
  /// Reports unavailable or corrupt assets and unsafe cache storage.
  pub async fn download_runtime(
    &self,
    cache_root: &Path,
    progress: impl Fn(DownloadProgress) + Send + Sync,
  ) -> Result<VerifiedBundle, Error> {
    self
      .resolver
      .archive_cached(
        &self.publication,
        self.target(),
        cache_root,
        Some((DownloadArtifact::Runtime, &progress)),
      )
      .await
  }
}
