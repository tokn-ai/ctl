//! Shared requests and results for explicit component updates.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Package {
  CtlAgent,
  #[default]
  FullBundle,
}

/// Named publication stream, independent of product and protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleChannel {
  Dev,
  Stable,
}

impl BundleChannel {
  #[must_use]
  pub const fn name(self) -> &'static str {
    match self {
      Self::Dev => "dev",
      Self::Stable => "stable",
    }
  }

  #[must_use]
  pub const fn configured_default() -> Self {
    match env!("CTL_BUNDLE_CHANNEL").as_bytes() {
      b"stable" => Self::Stable,
      _ => Self::Dev,
    }
  }
}

impl Default for BundleChannel {
  fn default() -> Self {
    Self::configured_default()
  }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildSource {
  #[default]
  Automatic,
  Published {
    channel: BundleChannel,
  },
  Selected,
  Provided {
    path: PathBuf,
    #[serde(default)]
    local_build: bool,
    ctld_package: Option<PathBuf>,
  },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateOptions {
  #[serde(default)]
  pub package: Package,
  #[serde(default)]
  pub source: BuildSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateResult {
  pub package: Package,
  pub bundle_id: String,
  pub target_triple: String,
  pub services_preserved: bool,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn update_source_defaults_to_automatic_and_channels_use_snake_case() {
    let options: UpdateOptions = serde_json::from_str("{}").unwrap();
    assert!(matches!(options.source, BuildSource::Automatic));
    for channel in [BundleChannel::Dev, BundleChannel::Stable] {
      let value = serde_json::to_value(BuildSource::Published { channel }).unwrap();
      assert_eq!(value["kind"], "published");
      assert_eq!(value["channel"], channel.name());
      assert!(
        matches!(serde_json::from_value::<BuildSource>(value).unwrap(), BuildSource::Published { channel: decoded } if decoded == channel)
      );
    }
    assert!(
      serde_json::from_str::<BuildSource>(r#"{"kind":"published","channel":"latest"}"#).is_err()
    );
  }
}

#[cfg(test)]
#[path = "../bundle_channel.rs"]
mod channel_policy;
