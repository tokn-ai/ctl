//! Checkout provenance is captured by Cargo, never inferred from the current directory.

use ctl_client::setup;
use ctl_core::protocol::ProtocolVersion;
use std::path::{Path, PathBuf};

struct DevelopmentContext {
  repository_root: &'static str,
  checkpoints: &'static [&'static str],
}

include!(concat!(env!("OUT_DIR"), "/development_ctld.rs"));

pub(super) fn enabled() -> bool {
  LOCAL_DEVELOPMENT.is_some()
}

pub(super) async fn discover(
  required: Option<ProtocolVersion>,
) -> Result<Option<PathBuf>, setup::Error> {
  let Some(context) = &LOCAL_DEVELOPMENT else {
    return Ok(None);
  };
  for checkpoint in context.checkpoints {
    if let Some(executable) = setup::discover_development_ctld(
      Path::new(checkpoint),
      Path::new(context.repository_root),
      required,
    )
    .await?
    {
      return Ok(Some(executable));
    }
  }
  Ok(None)
}
