use super::{
  BUNDLE_SET_FILE, BundleChannel, Error, MAX_BUNDLE_SET_BYTES, PublishedTarget, Resolver,
  is_prerelease, parse_published_manifest,
};
use crate::remote_bundle::{safe_id, valid_bundle_id};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_RELEASE_CATALOG_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct PublishedRelease {
  pub channel: BundleChannel,
  pub publication_id: String,
  pub bundle_id: String,
  pub app_version: String,
  pub git_revision: String,
  pub current: bool,
  pub built_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishedReleaseDetails {
  #[serde(flatten)]
  pub release: PublishedRelease,
  pub targets: Vec<PublishedTarget>,
}

/// Lists the current release for a published channel.
/// # Errors
/// Reports catalog download and validation errors.
pub async fn list_releases(channel: BundleChannel) -> Result<Vec<PublishedRelease>, Error> {
  Resolver::production()?.list_releases(channel).await
}

/// Inspects the current release, optionally requiring its exact publication ID.
/// # Errors
/// Reports unavailable publications and metadata download or validation errors.
pub async fn view_release(
  channel: BundleChannel,
  publication_id: Option<&str>,
) -> Result<PublishedReleaseDetails, Error> {
  Resolver::production()?
    .view_release(channel, publication_id)
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseCatalog {
  schema_version: u32,
  site: String,
  generation: u64,
  publications: Vec<RetainedPublication>,
  channels: ChannelHeads,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelHeads {
  nightly: Option<String>,
  dev: Option<String>,
  stable: Option<String>,
}

impl ChannelHeads {
  fn get(&self, channel: BundleChannel) -> Option<&str> {
    match channel {
      BundleChannel::Nightly => self.nightly.as_deref().or(self.dev.as_deref()),
      BundleChannel::Stable => self.stable.as_deref(),
    }
  }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedPublication {
  channel: BundleChannel,
  publication_id: String,
  bundle_id: String,
  app_version: String,
  git_revision: String,
  run_id: String,
  run_number: u64,
  run_attempt: u64,
}

impl RetainedPublication {
  fn valid_identity(&self) -> bool {
    if !safe_id(&self.app_version)
      || !safe_id(&self.bundle_id)
      || self.git_revision.len() != 40
      || !self
        .git_revision
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
      || self.run_id.starts_with('0')
      || !self.run_id.bytes().all(|byte| byte.is_ascii_digit())
      || self.run_id.parse::<u64>().is_err()
      || self.run_number == 0
      || self.run_attempt == 0
    {
      return false;
    }
    (self.channel == BundleChannel::Stable) == (self.bundle_id == self.app_version)
      && valid_bundle_id(&self.app_version, &self.bundle_id, &self.git_revision)
      && self.publication_id == format!("{}-{}-{}", self.bundle_id, self.run_id, self.run_attempt)
  }

  fn belongs_to(&self, channel: BundleChannel) -> bool {
    self.channel == channel
      && (channel != BundleChannel::Stable || !is_prerelease(&self.app_version))
  }

  fn release(&self, current: Option<&str>) -> PublishedRelease {
    PublishedRelease {
      channel: self.channel,
      publication_id: self.publication_id.clone(),
      bundle_id: self.bundle_id.clone(),
      app_version: self.app_version.clone(),
      git_revision: self.git_revision.clone(),
      current: current == Some(self.publication_id.as_str()),
      built_at: None,
    }
  }
}

impl ReleaseCatalog {
  fn parse(bytes: &[u8]) -> Result<Self, Error> {
    let catalog: Self = serde_json::from_slice(bytes)
      .map_err(|_| Error::Invalid("invalid component release catalog".into()))?;
    if catalog.schema_version != 1
      || catalog.site != "ctl-bundle-pages"
      || catalog.generation == 0
      || catalog.publications.len() > 64
    {
      return Err(Error::Invalid(
        "unsupported component release catalog".into(),
      ));
    }
    let mut ids = BTreeSet::new();
    if catalog
      .publications
      .iter()
      .any(|entry| !entry.valid_identity() || !ids.insert(&entry.publication_id))
    {
      return Err(Error::Invalid(
        "invalid or duplicate catalog publication identity".into(),
      ));
    }
    if catalog
      .channels
      .nightly
      .as_ref()
      .zip(catalog.channels.dev.as_ref())
      .is_some_and(|(nightly, dev)| nightly != dev)
      || [BundleChannel::Nightly, BundleChannel::Stable]
        .into_iter()
        .any(|channel| {
          catalog.channels.get(channel).is_some_and(|head| {
            !catalog
              .publications
              .iter()
              .any(|entry| entry.belongs_to(channel) && entry.publication_id == head)
          })
        })
    {
      return Err(Error::Invalid(
        "invalid channel head in component release catalog".into(),
      ));
    }
    Ok(catalog)
  }
}

impl Resolver {
  async fn release_catalog(&self) -> Result<ReleaseCatalog, Error> {
    let url = self
      .root
      .join("catalog.json")
      .map_err(|error| Error::Invalid(error.to_string()))?;
    ReleaseCatalog::parse(&self.read(&url, MAX_RELEASE_CATALOG_BYTES).await?)
  }

  pub(super) async fn list_releases(
    &self,
    channel: BundleChannel,
  ) -> Result<Vec<PublishedRelease>, Error> {
    let catalog = self.release_catalog().await?;
    let Some(id) = catalog.channels.get(channel) else {
      return Ok(Vec::new());
    };
    let entry = catalog
      .publications
      .iter()
      .find(|entry| entry.belongs_to(channel) && entry.publication_id == id)
      .ok_or_else(|| Error::Invalid("channel head is absent from its catalog".into()))?;
    let url = self
      .root
      .join(&format!("sets/{id}/bundle-set.json"))
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let mut release = entry.release(Some(id));
    release.built_at = self
      .build_time_for_identity(
        &url,
        &entry.app_version,
        &entry.bundle_id,
        &entry.git_revision,
      )
      .await?;
    Ok(vec![release])
  }

  pub(super) async fn view_release(
    &self,
    channel: BundleChannel,
    publication_id: Option<&str>,
  ) -> Result<PublishedReleaseDetails, Error> {
    let catalog = self.release_catalog().await?;
    let current = catalog.channels.get(channel);
    let id = publication_id.or(current).ok_or_else(|| {
      Error::NotAvailable(format!(
        "the {} channel has no current release",
        channel.name()
      ))
    })?;
    let entry = catalog
      .publications
      .iter()
      .find(|entry| entry.belongs_to(channel) && entry.publication_id == id && current == Some(id))
      .ok_or_else(|| {
        Error::NotAvailable(format!(
          "the requested {} release is not the current publication",
          channel.name()
        ))
      })?;
    self.release_details(entry, current).await
  }

  async fn release_details(
    &self,
    entry: &RetainedPublication,
    current: Option<&str>,
  ) -> Result<PublishedReleaseDetails, Error> {
    let url = self
      .root
      .join(&format!("sets/{}/bundle-set.json", entry.publication_id))
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let manifest = parse_published_manifest(
      &self.read(&url, MAX_BUNDLE_SET_BYTES).await?,
      BUNDLE_SET_FILE,
    )?;
    if manifest.app_version != entry.app_version
      || manifest.bundle_id != entry.bundle_id
      || manifest.git_revision != entry.git_revision
    {
      return Err(Error::Invalid(
        "release catalog and bundle manifest identities differ".into(),
      ));
    }
    let mut release = entry.release(current);
    release.built_at = self.build_time(&url, &manifest).await?;
    Ok(PublishedReleaseDetails {
      release,
      targets: self.catalog_targets(&url, &manifest).await?,
    })
  }
}
