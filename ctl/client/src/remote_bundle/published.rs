//! Discover named component channels without changing any installed selection.

use ctl_core::component::ComponentInfo;
use ctl_core::component_update::BundleChannel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

use super::{BundleSet, Error, MAX_BUNDLE_BYTES, MAX_BUNDLE_SET_BYTES, VerifiedBundle};

pub const ROOT: &str = "https://tokn-ai.dev/ctl/bundles/";
const MAX_CHANNEL_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct PublishedTarget {
  pub target_triple: String,
  pub compatible: bool,
  pub components: BTreeMap<String, ComponentInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishedCatalog {
  pub channel: BundleChannel,
  pub bundle_id: String,
  pub app_version: String,
  pub git_revision: String,
  pub targets: Vec<PublishedTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelDocument {
  schema_version: u32,
  channel: BundleChannel,
  bundle_set_url: String,
}

struct Publication {
  manifest: BundleSet,
  bytes: Vec<u8>,
  url: reqwest::Url,
}

struct Resolver {
  root: reqwest::Url,
  client: reqwest::Client,
}

#[must_use]
pub const fn default_channel() -> BundleChannel {
  BundleChannel::configured_default()
}

/// Checks publication metadata only: no archives, imports, selections or SSH.
///
/// # Errors
/// Rejects unavailable channels and malformed or untrusted publication metadata.
pub async fn check(channel: BundleChannel) -> Result<PublishedCatalog, Error> {
  Resolver::production()?.check(channel).await
}

/// Downloads exactly the immutable publication currently named by a channel.
///
/// # Errors
/// Rejects wrong channels, incompatible components, unsafe URLs and corrupt bytes.
pub async fn download(channel: BundleChannel, target: &str) -> Result<VerifiedBundle, Error> {
  Resolver::production()?.download(channel, target).await
}

/// Resolve the channel afresh, reusing only a checksum-verified exact manifest.
/// A network failure never selects a cached older channel publication.
///
/// # Errors
/// Reports discovery, validation or unsafe cache failures without installing anything.
pub async fn download_cached(
  channel: BundleChannel,
  target: &str,
  cache_root: &std::path::Path,
) -> Result<VerifiedBundle, Error> {
  Resolver::production()?
    .download_cached(channel, target, cache_root)
    .await
}

impl Resolver {
  fn production() -> Result<Self, Error> {
    Self::new(
      reqwest::Url::parse(ROOT).expect("fixed publication URL"),
      true,
    )
  }

  fn new(root: reqwest::Url, https_only: bool) -> Result<Self, Error> {
    let redirects = root.clone();
    let builder = reqwest::Client::builder();
    #[cfg(test)]
    let builder = if https_only {
      builder
    } else {
      builder.no_proxy()
    };
    let client = builder
      .https_only(https_only)
      .connect_timeout(Duration::from_secs(15))
      .read_timeout(Duration::from_secs(30))
      .redirect(reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 || !trusted(attempt.url(), &redirects) {
          attempt.error("untrusted component publication redirect")
        } else {
          attempt.follow()
        }
      }))
      .user_agent(concat!("ctl/", env!("CARGO_PKG_VERSION")))
      .build()
      .map_err(super::download_error)?;
    Ok(Self { root, client })
  }

  async fn publication(&self, channel: BundleChannel) -> Result<Publication, Error> {
    let channel_url = self
      .root
      .join(&format!("channels/{}.json", channel.name()))
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let pointer: ChannelDocument =
      serde_json::from_slice(&self.read(&channel_url, MAX_CHANNEL_BYTES).await?)
        .map_err(|_| Error::Invalid("invalid component channel document".into()))?;
    if pointer.schema_version != 1 || pointer.channel != channel {
      return Err(Error::Invalid(
        "component channel identity does not match the request".into(),
      ));
    }
    let url = reqwest::Url::parse(&pointer.bundle_set_url)
      .map_err(|_| Error::Invalid("invalid component publication URL".into()))?;
    if !trusted(&url, &self.root) || !url.path().ends_with("/bundle-set.json") {
      return Err(Error::Invalid("untrusted component publication URL".into()));
    }
    let bytes = self.read(&url, MAX_BUNDLE_SET_BYTES).await?;
    let manifest = BundleSet::parse_intrinsic(&bytes)?;
    if manifest.schema_version != 2 {
      return Err(Error::Invalid(
        "published channels require schema-2 component metadata".into(),
      ));
    }
    if channel == BundleChannel::Stable
      && (manifest.bundle_id != manifest.app_version
        || manifest
          .app_version
          .split('+')
          .next()
          .unwrap_or(&manifest.app_version)
          .contains('-'))
    {
      return Err(Error::Invalid(
        "stable channel points to a development or prerelease bundle".into(),
      ));
    }
    Ok(Publication {
      manifest,
      bytes,
      url,
    })
  }

  async fn check(&self, channel: BundleChannel) -> Result<PublishedCatalog, Error> {
    let publication = self.publication(channel).await?;
    let manifest = publication.manifest;
    let targets = manifest
      .targets
      .iter()
      .map(|(target, metadata)| {
        Ok(PublishedTarget {
          target_triple: target.clone(),
          compatible: manifest.is_compatible(target)?,
          components: metadata
            .components
            .clone()
            .ok_or_else(|| Error::Invalid("publication lacks component metadata".into()))?,
        })
      })
      .collect::<Result<_, Error>>()?;
    Ok(PublishedCatalog {
      channel,
      bundle_id: manifest.bundle_id,
      app_version: manifest.app_version,
      git_revision: manifest.git_revision,
      targets,
    })
  }

  async fn download(&self, channel: BundleChannel, target: &str) -> Result<VerifiedBundle, Error> {
    let publication = self.publication(channel).await?;
    if !publication.manifest.is_compatible(target)? {
      return Err(Error::NotAvailable(
        "published components do not support this client's contracts".into(),
      ));
    }
    let archive_url = publication
      .url
      .join(publication.manifest.archive_name(target)?)
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let archive = self.read(&archive_url, MAX_BUNDLE_BYTES).await?;
    publication
      .manifest
      .verify_archive(target, archive, publication.bytes)
  }

  async fn download_cached(
    &self,
    channel: BundleChannel,
    target: &str,
    cache_root: &std::path::Path,
  ) -> Result<VerifiedBundle, Error> {
    let publication = self.publication(channel).await?;
    if !publication.manifest.is_compatible(target)? {
      return Err(Error::NotAvailable(
        "published components do not support this client's contracts".into(),
      ));
    }
    let expected = &publication
      .manifest
      .target(target)?
      .components
      .as_ref()
      .ok_or_else(|| Error::Invalid("publication lacks component metadata".into()))?["ctl-agent"]
      .build;
    let cache =
      super::BundleCacheEntry::publication(cache_root, target, expected, &publication.bytes)?;
    let read_cache = cache.clone();
    if let Some(bundle) = tokio::task::spawn_blocking(move || read_cache.load())
      .await
      .map_err(|error| Error::Io(std::io::Error::other(error)))??
    {
      if bundle.manifest != publication.bytes {
        return Err(Error::Invalid(
          "cached publication differs from its content identity".into(),
        ));
      }
      return Ok(bundle);
    }
    let archive_url = publication
      .url
      .join(publication.manifest.archive_name(target)?)
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let archive = self.read(&archive_url, MAX_BUNDLE_BYTES).await?;
    let bundle = publication
      .manifest
      .verify_archive(target, archive, publication.bytes)?;
    tokio::task::spawn_blocking(move || {
      cache.store(&bundle)?;
      Ok(bundle)
    })
    .await
    .map_err(|error| Error::Io(std::io::Error::other(error)))?
  }

  async fn read(&self, url: &reqwest::Url, maximum: usize) -> Result<Vec<u8>, Error> {
    if !trusted(url, &self.root) {
      return Err(Error::Invalid("untrusted component publication URL".into()));
    }
    let response = self
      .client
      .get(url.clone())
      .header(reqwest::header::CACHE_CONTROL, "no-cache")
      .send()
      .await
      .map_err(super::download_error)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
      return Err(Error::NotAvailable(
        "this component channel or publication is not published".into(),
      ));
    }
    let mut response = response.error_for_status().map_err(super::download_error)?;
    let length = response.content_length();
    if length.is_some_and(|length| length > maximum as u64) {
      return Err(Error::Invalid(
        "component publication exceeds its size limit".into(),
      ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(super::download_error)? {
      if chunk.len() > maximum.saturating_sub(bytes.len()) {
        return Err(Error::Invalid(
          "component publication exceeds its size limit".into(),
        ));
      }
      bytes.extend_from_slice(&chunk);
    }
    if length.is_some_and(|length| length != bytes.len() as u64) {
      return Err(Error::Invalid("truncated component publication".into()));
    }
    Ok(bytes)
  }
}

fn trusted(url: &reqwest::Url, root: &reqwest::Url) -> bool {
  url.origin() == root.origin()
    && url.username().is_empty()
    && url.password().is_none()
    && url.query().is_none()
    && url.fragment().is_none()
    && url.path().starts_with(root.path())
    && url
      .path()
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || b"/._+-".contains(&byte))
}

#[cfg(test)]
mod tests;
