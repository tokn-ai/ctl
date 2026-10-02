use ctl_client::setup::{self, SetupEvent, SetupOutcome};

// Ordinary Cargo builds embed no payload, so neither mode is constructed there.
#[expect(
  dead_code,
  reason = "signing mode is selected by the embedded build payload"
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
  ctl_ipc::register_standalone_daemon_executable_provider(|| Box::pin(prepare()))
}

async fn prepare() -> std::io::Result<Option<std::path::PathBuf>> {
  let mut waiting = false;
  loop {
    match prepare_once().await {
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

async fn prepare_once() -> Result<Option<std::path::PathBuf>, setup::Error> {
  if let Some(executable) = setup::discover_compatible_ctld().await? {
    return Ok(Some(executable));
  }
  let Some(bundle) = BUNDLED_CTLD else {
    return Ok(None);
  };
  let outcome = bundle
    .install(|event| {
      if matches!(event, SetupEvent::Extracting) {
        eprintln!("Preparing bundled ctld...");
      }
    })
    .await?;
  Ok(Some(outcome.executable))
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
