//! Package development helpers and renew them using the installed build.

use super::{OutputDirectory, PreparedDevelopment, invalid, prepared_result};
use crate::components::macos_runtime::SignedHelper;
use ctl_core::bundles::{Bundle, Manifest, Source};
use ctl_core::component::ComponentInfo;
use ctl_core::installations::{Installations, Lease};
use ctl_core::private_fs;
use std::io::{self, Write as _};
use std::path::Path;

const RECEIPT: &str = "ctld-package.json";
const HELPER: &str = "ctld.app/Contents/MacOS/ctld";

/// Writes and returns the receipt for a staged development `ctld.app`.
/// The caller must verify the resulting package before using it.
///
/// # Errors
/// Reports an incomplete helper, invalid component identity, or unsafe output.
pub fn write_development_helper_receipt(
  output_directory: &Path,
  component: &ComponentInfo,
  target: &str,
  team: &str,
) -> io::Result<Vec<u8>> {
  let signed = SignedHelper::read(
    &output_directory.join("ctld.app"),
    component,
    target,
    team,
    true,
  )?;
  write_private(
    &output_directory.join("installation.json"),
    &signed.receipt,
    false,
  )?;
  Ok(signed.receipt)
}

/// Re-signs the active macOS development helper and selects a verified copy of
/// its runtime bundle. The active executable is never run during preparation.
/// The signer receives the staged executable, output app path, app version and
/// the installed Apple Team ID. After verification, `before_activate` can stop
/// the transaction if the caller has been interrupted.
///
/// # Errors
/// Reports missing or modified installations, signing or verification errors,
/// and concurrent activation of a different build.
pub async fn renew_signing(
  home: &Path,
  sign: impl AsyncFnOnce(&Path, &Path, &str, &str) -> io::Result<()>,
  before_activate: impl FnOnce() -> io::Result<()>,
) -> io::Result<PreparedDevelopment> {
  let installations = Installations::new(home);
  let active = installations.active()?.ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::NotFound,
      "No active local development build. Run ctlup dev to install one, or use ctlup sign --version <VERSION> for a CI release.",
    )
  })?;
  let manifest = &active.manifest;
  if manifest.source != Source::Local {
    return Err(invalid(
      "The active runtime is not a local development build. Use ctlup sign --version <VERSION> for a CI release.",
    ));
  }
  let mut files = active.read_files()?;
  let component = &manifest.components["ctld"];
  let target = &manifest.target_triple;
  let receipt = files
    .get(RECEIPT)
    .ok_or_else(|| invalid("the active build has no signed development helper receipt"))?;
  let team =
    crate::setup::development_signing_team(receipt, target, component).map_err(io::Error::other)?;
  // Use the verified executable from the signed app. Expiry can prevent even
  // --component-info from running. Verify the metadata only after signing.
  let original = files
    .remove(HELPER)
    .ok_or_else(|| invalid("the active build has no signed development helper"))?;
  files.retain(|name, _| name != "ctld" && name != RECEIPT && !name.starts_with("ctld.app/"));

  let staging = installations.updater_root().join("staging");
  crate::installation::safe_directory(&staging)?;
  let output = staging.join(format!("ctld-renewal-{}", uuid::Uuid::new_v4()));
  private_fs::create_directory(&output)?;
  let output = OutputDirectory(output);
  let artifact = output.0.join("ctld");
  write_private(&artifact, &original, true)?;
  drop(original);
  let app = output.0.join("ctld.app");
  sign(&artifact, &app, &component.build.version, &team).await?;

  let mut executables = manifest
    .files
    .iter()
    .filter(|(name, info)| info.executable && files.contains_key(*name))
    .map(|(name, _)| name.clone())
    .collect::<Vec<_>>();
  SignedHelper::read(&app, component, target, &team, true)?.add_to(&mut files, &mut executables);
  let mut replacement =
    Manifest::new(target, manifest.source, manifest.components.clone(), &files)?
      .with_executables(&executables)?;
  if let Some(id) = &manifest.distribution_id {
    replacement = replacement.with_distribution_id(id)?;
  }
  // Verify the exact repackaged bytes in private staging before making the
  // replacement visible to installation readers.
  let bundle = Bundle::stage(&output.0, &replacement, &files)?;
  drop(files);
  crate::components::verify_local(home, &bundle).await?;
  before_activate()?;

  let (installed, lease) =
    activate(installations, bundle, active.manifest.bundle_id, output).await?;
  Ok(prepared_result(home, &installed, lease))
}

async fn activate(
  installations: Installations,
  bundle: Bundle,
  expected: String,
  output: OutputDirectory,
) -> io::Result<(Bundle, Lease)> {
  tokio::task::spawn_blocking(move || {
    // Keep staging available if the outer future is cancelled mid-activation.
    let activated = installations.activate_renewed_helper(&bundle, &expected);
    drop(bundle);
    drop(output);
    let (installed, lease) = activated?;
    if installed.manifest.bundle_id != expected
      && let Err(error) = installations.prune_unused()
    {
      eprintln!("ctlup: renewed ctld signing successfully; cleanup deferred: {error}");
    }
    Ok((installed, lease))
  })
  .await
  .map_err(io::Error::other)?
}

fn write_private(path: &Path, bytes: &[u8], executable: bool) -> io::Result<()> {
  let mut file = private_fs::options()
    .write(true)
    .create_new(true)
    .open(path)?;
  file.write_all(bytes)?;
  if executable {
    private_fs::set_executable(&file)?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::remote_bundle::compatibility::tests::Fixture;
  use ctl_core::bundles::Store;
  use ctl_core::installations::CatalogOrigin;
  use std::fs;

  #[tokio::test]
  async fn failed_renewal_passes_verified_bytes_and_installed_team_to_the_signer() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let home = OutputDirectory::new().unwrap();
    let fixture = Fixture::new("0.1.0", &"a".repeat(40)).with_clients();
    let components: std::collections::BTreeMap<String, ComponentInfo> = serde_json::from_value(
      fixture.outer["targets"]["aarch64-apple-darwin"]["components"].clone(),
    )
    .unwrap();
    let component = &components["ctld"];
    let target = ctl_core::paths::native_target();
    let old_helper = b"#!/bin/sh\nexit 19\n";
    let mut files = fixture.files;
    files.insert("ctld".into(), old_helper.to_vec());
    files.insert(HELPER.into(), old_helper.to_vec());
    files.insert(
      RECEIPT.into(),
      crate::setup::create_runtime_helper_receipt(
        component,
        target,
        "ABCDEFGHIJ",
        true,
        "d".repeat(64),
        1,
      )
      .unwrap(),
    );
    let manifest = Manifest::new(target, Source::Local, components, &files).unwrap();
    let pending = Store::cache(&home.0).publish(&manifest, &files).unwrap();
    let installations = Installations::new(&home.0);
    let installed = installations
      .activate_with_origin(
        &pending,
        &CatalogOrigin {
          channel: "local".into(),
          publication_id: Some("local.test".into()),
          built_at: None,
        },
      )
      .unwrap();
    let before = installations.catalog().unwrap();

    let mut staging = None;
    let error = renew_signing(
      &home.0,
      async |artifact, app, version, team| {
        assert_eq!(version, "0.1.0");
        assert_eq!(team, "ABCDEFGHIJ");
        assert_ne!(artifact, installed.directory.join(HELPER));
        assert_eq!(fs::read(artifact).unwrap(), old_helper);
        let metadata = fs::metadata(artifact).unwrap();
        assert!(private_fs::is_private_file(&metadata));
        assert!(private_fs::is_executable(&metadata));
        assert_eq!(app, artifact.with_file_name("ctld.app"));
        assert!(artifact.starts_with(installations.updater_root().join("staging")));
        staging = artifact.parent().map(Path::to_path_buf);
        Err(io::Error::other("fixture signing failure"))
      },
      || unreachable!("a failed signer must prevent activation"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "fixture signing failure");
    assert!(!staging.unwrap().exists());
    assert_eq!(installations.active().unwrap().unwrap().manifest, manifest);
    assert_eq!(installations.catalog().unwrap(), before);
    assert_eq!(installed.read_files().unwrap(), files);
  }
}
