//! Executable-local inputs for the shared ctld resolver.

use ctl_client::ctld::Resolver;

include!(concat!(env!("OUT_DIR"), "/bundled_ctld.rs"));
include!(concat!(env!("OUT_DIR"), "/development_ctld.rs"));

pub(crate) fn register() -> std::io::Result<()> {
  ctl_ipc::resolver::register(|request| {
    Box::pin(async move {
      Resolver::standalone(BUNDLED_CTLD, LOCAL_DEVELOPMENT.as_ref())
        .resolve(request)
        .await
    })
  })
}
