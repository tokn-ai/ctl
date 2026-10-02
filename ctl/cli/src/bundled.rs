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
  if BUNDLED_CTLD.is_some() {
    ctl_ipc::register_daemon_executable_provider(|| Box::pin(prepare()))?;
  }
  Ok(())
}

async fn prepare() -> std::io::Result<Option<std::path::PathBuf>> {
  let bundle = BUNDLED_CTLD.unwrap();
  let mut waiting = false;
  loop {
    let result = bundle
      .install(|event| {
        if matches!(event, SetupEvent::Extracting) {
          eprintln!("Preparing bundled ctld...");
        }
      })
      .await;
    match result {
      Ok(outcome) => return Ok(Some(outcome.executable)),
      Err(setup::Error::Busy) => {
        // Another CLI may be preparing this same immutable bundle. The OS lock
        // is released even if that process exits; every verification operation
        // is bounded. Keep cancellation available while waiting to reuse it.
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
