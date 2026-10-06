use ctl_client::setup::{self, SetupEvent, SetupOutcome};
use ctl_core::protocol::ProtocolVersion;
use std::future::Future;
use std::path::PathBuf;

mod local;

// Ordinary Cargo builds embed no payload, so neither mode is constructed there.
#[cfg_attr(
  not(test),
  expect(
    dead_code,
    reason = "signing mode is selected by the embedded build payload"
  )
)]
#[derive(Clone, Copy)]
enum BundleMode {
  Signed,
  Development,
}

#[derive(Clone, Copy)]
struct EmbeddedBundle {
  manifest: &'static [u8],
  archive: &'static [u8],
  mode: BundleMode,
}

impl EmbeddedBundle {
  async fn install(
    self,
    on_progress: impl Fn(SetupEvent) + Send + Sync,
  ) -> Result<SetupOutcome, setup::Error> {
    match self.mode {
      BundleMode::Signed => {
        setup::install_bundled_ctld(self.manifest, self.archive, on_progress).await
      }
      BundleMode::Development => {
        setup::install_bundled_development_ctld(self.manifest, self.archive, on_progress).await
      }
    }
  }
}

include!(concat!(env!("OUT_DIR"), "/bundled_ctld.rs"));

pub fn register() -> std::io::Result<()> {
  let register = if is_development() {
    ctl_ipc::register_development_daemon_executable_provider
  } else if local::enabled() {
    ctl_ipc::register_preferred_contract_daemon_executable_provider
  } else {
    ctl_ipc::register_contract_daemon_executable_provider
  };
  register(|required| Box::pin(prepare(required)))
}

async fn prepare(required: Option<ProtocolVersion>) -> std::io::Result<Option<PathBuf>> {
  let mut waiting = false;
  loop {
    match prepare_once(required).await {
      Ok(executable) => return Ok(executable),
      Err(setup::Error::Busy) => {
        // Another CLI may be selecting or preparing a shared installation. The
        // OS lock is released even if that process exits; every verification
        // operation is bounded. Keep cancellation available while waiting.
        if !waiting {
          eprintln!("Waiting for another ctld setup to finish...");
          waiting = true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
      }
      Err(error) => return Err(std::io::Error::other(error)),
    }
  }
}

async fn prepare_once(required: Option<ProtocolVersion>) -> Result<Option<PathBuf>, setup::Error> {
  let shared = async {
    match required {
      Some(required) => setup::discover_ctld_for_helper_contract(required).await,
      None => setup::discover_compatible_ctld().await,
    }
  };
  select_helper(
    BUNDLED_CTLD,
    local::discover(required),
    shared,
    |bundle| async move {
      let on_progress = |event| {
        if matches!(event, SetupEvent::Extracting) {
          eprintln!("Preparing bundled ctld...");
        }
      };
      let outcome = match bundle.mode {
        BundleMode::Development => {
          setup::prepare_bundled_development_ctld(bundle.manifest, bundle.archive, on_progress)
            .await?
        }
        BundleMode::Signed => bundle.install(on_progress).await?,
      };
      Ok(outcome.executable)
    },
  )
  .await
}

async fn select_helper<F>(
  bundle: Option<EmbeddedBundle>,
  local: impl Future<Output = Result<Option<PathBuf>, setup::Error>>,
  shared: impl Future<Output = Result<Option<PathBuf>, setup::Error>>,
  prepare: impl FnOnce(EmbeddedBundle) -> F,
) -> Result<Option<PathBuf>, setup::Error>
where
  F: Future<Output = Result<PathBuf, setup::Error>>,
{
  if let Some(bundle) = bundle.filter(|bundle| matches!(bundle.mode, BundleMode::Development)) {
    return prepare(bundle).await.map(Some);
  }
  if bundle.is_none()
    && let Some(executable) = local.await?
  {
    return Ok(Some(executable));
  }
  if let Some(executable) = shared.await? {
    return Ok(Some(executable));
  }
  match bundle {
    Some(bundle) => prepare(bundle).await.map(Some),
    None => Ok(None),
  }
}

pub async fn install(
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, setup::Error> {
  if let Some(bundle) = BUNDLED_CTLD {
    bundle.install(on_progress).await
  } else {
    setup::install_signed_ctld(on_progress).await
  }
}

pub fn is_development() -> bool {
  BUNDLED_CTLD.is_some_and(|bundle| matches!(bundle.mode, BundleMode::Development))
}

#[cfg(test)]
mod tests;
