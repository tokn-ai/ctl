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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildSource {
  #[default]
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
