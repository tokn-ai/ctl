//! Private SSH deployment entry points. Stdout is the install-progress protocol.

use ctl_core::installations::CatalogOrigin;
use std::io::{self, Write as _};
use std::path::Path;
use tokio::io::AsyncReadExt as _;

pub(super) async fn install(home: &Path, origin: Option<CatalogOrigin>) -> io::Result<()> {
  emit(ctl_client::RemoteInstallEvent::Receiving { received_bytes: 0 });
  let mut input = tokio::io::stdin();
  let mut bytes = Vec::new();
  let mut chunk = vec![0; 64 * 1024];
  loop {
    let count = input.read(&mut chunk).await?;
    if count == 0 {
      break;
    }
    if count > ctl_core::bundles::MAX_FILE_BYTES.saturating_sub(bytes.len()) {
      return Err(io::Error::other(
        "installation archive exceeds its size limit",
      ));
    }
    bytes.extend_from_slice(&chunk[..count]);
    emit(ctl_client::RemoteInstallEvent::Receiving {
      received_bytes: bytes.len() as u64,
    });
  }
  ctl_client::installation::install_archive_with_origin(home, &bytes, origin, emit).await?;
  Ok(())
}

pub(super) async fn activate(
  home: &Path,
  bundle_id: &str,
  origin: Option<CatalogOrigin>,
) -> io::Result<i32> {
  let home = home.to_owned();
  let bundle_id = bundle_id.to_owned();
  tokio::task::spawn_blocking(move || {
    let installs = ctl_core::installations::Installations::new(&home);
    let bundle = match installs.get(&bundle_id) {
      Ok(bundle) => bundle,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(10),
      Err(error) => return Err(error),
    };
    let (_, _lease) = ctl_client::installation::activate_uploaded(&home, &bundle, origin.as_ref())?;
    if let Err(error) = installs.prune() {
      eprintln!("ctlup: activated successfully; cleanup deferred: {error}");
    }
    emit(ctl_client::RemoteInstallEvent::Complete);
    Ok(0)
  })
  .await
  .map_err(io::Error::other)?
}

fn emit(event: ctl_client::RemoteInstallEvent) {
  use ctl_client::RemoteInstallEvent as Event;
  let line = match event {
    Event::SendingUpdater { .. } | Event::CheckingUpdater => return,
    Event::Receiving { received_bytes } => {
      format!("ctl-install-progress-v1 receiving {received_bytes}")
    }
    Event::Extracting => "ctl-install-progress-v1 extracting".into(),
    Event::Checking { file_name } => format!("ctl-install-progress-v1 checking {file_name}"),
    Event::Activating => "ctl-install-progress-v1 activating".into(),
    Event::Complete => "ctl-install-v1".into(),
  };
  let mut output = io::stdout().lock();
  let _ = writeln!(output, "{line}");
  let _ = output.flush();
}
