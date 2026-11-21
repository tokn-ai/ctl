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

pub(super) async fn install_publication(
  home: &Path,
  request: super::arguments::PublicationArguments,
) -> io::Result<i32> {
  let origin = ctl_client::installation::decode_catalog_origin(&request.origin_hex)?;
  let manifest_bytes = request.manifest_bytes;
  if !(1..=ctl_client::remote_bundle::MAX_BUNDLE_SET_BYTES).contains(&manifest_bytes) {
    return Err(io::Error::other("invalid publication manifest length"));
  }
  let mut input = tokio::io::stdin();
  let mut manifest = vec![0; manifest_bytes];
  input.read_exact(&mut manifest).await?;
  let archive = if request.archive_stdin {
    let mut archive = Vec::new();
    let maximum = ctl_core::bundles::MAX_FILE_BYTES;
    input
      .take(maximum as u64 + 1)
      .read_to_end(&mut archive)
      .await?;
    if archive.len() > maximum {
      return Err(io::Error::other("published archive exceeds its size limit"));
    }
    Some(archive)
  } else {
    None
  };
  let Some((installed, reused)) = ctl_client::remote_bundle::install_host_publication(
    home,
    &request.target,
    &request.archive_url,
    origin,
    manifest,
    archive,
  )
  .await?
  else {
    println!("ctl-published-unavailable-v1");
    return Ok(12);
  };
  if reused {
    println!("ctl-published-reused-v1");
  }
  println!(
    "ctl-published-install-v1\n{}",
    serde_json::to_string(&installed).map_err(io::Error::other)?
  );
  Ok(0)
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
    Event::SendingUpdater { .. }
    | Event::CheckingUpdater
    | Event::CheckingRemoteInstallation
    | Event::Reused => return,
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
