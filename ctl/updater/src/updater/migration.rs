//! One explicit metadata cutover preserves existing complete runtime payloads.

use ctl_client::installation::InstallationCatalog;
use ctl_core::installations::Installations;
use std::io;
use std::path::PathBuf;

pub(super) async fn run(home: PathBuf, json: bool) -> io::Result<()> {
  let entries = tokio::task::spawn_blocking(move || Installations::new(&home).migrate_catalog())
    .await
    .map_err(io::Error::other)??;
  let catalog = InstallationCatalog::new(entries);
  catalog.validate()?;
  if json {
    println!(
      "{}",
      serde_json::to_string(&catalog).map_err(io::Error::other)?
    );
  } else {
    println!(
      "Recorded {} installed build memberships in the channel catalog.",
      catalog.installations.len()
    );
    println!("Run `ctlup component list --installed` or `ctlup show` to inspect them.");
  }
  Ok(())
}
