//! Checkout provenance is captured by Cargo, never inferred from the current directory.

include!(concat!(env!("OUT_DIR"), "/development_ctld.rs"));

pub(super) fn context() -> Option<&'static ctl_client::setup::DevelopmentContext> {
  LOCAL_DEVELOPMENT.as_ref()
}
