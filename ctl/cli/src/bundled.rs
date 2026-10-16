//! Build inputs for the shared ctld resolver; selection policy lives in ctl-client.

use ctl_client::ctld::{EmbeddedBundle, Resolver};
use ctl_ipc::resolver::{Request, Selection};

mod local;

include!(concat!(env!("OUT_DIR"), "/bundled_ctld.rs"));

pub fn register() -> std::io::Result<()> {
  ctl_ipc::resolver::register(|request| Box::pin(resolve(request)))
}

async fn resolve(request: Request) -> std::io::Result<Option<Selection>> {
  Resolver::standalone(BUNDLED_CTLD, local::context())
    .resolve(request)
    .await
}
