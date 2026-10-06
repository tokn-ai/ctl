//! Checkout provenance is captured by Cargo, never inferred from the current directory.

use ctl_client::setup;
use ctl_core::protocol::ProtocolVersion;
use std::path::PathBuf;

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
  context.discover(required).await
}
