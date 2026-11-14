//! Replace an expired development helper signature using the installed build.

use super::{
  MAX_DESCRIPTOR_BYTES, OutputDirectory, PreparedDevelopment, invalid, prepared_result,
  recipe_command, run_recipe,
};
use ctl_core::bundles::{Bundle, Manifest, Source, Store};
use ctl_core::component::ComponentInfo;
use ctl_core::installations::{Installations, Lease};
use ctl_core::private_fs;
use serde::Serialize;
use std::io::{self, Write as _};
use std::path::Path;

const RECEIPT: &str = "ctld-package.json";
const HELPER: &str = "ctld.app/Contents/MacOS/ctld";

#[derive(Serialize)]
struct SigningInput<'a> {
  artifact: &'a Path,
  target: &'a str,
  expected_component: &'a ComponentInfo,
  expected_team: &'a str,
  output_directory: &'a Path,
}

/// Re-signs the active macOS development helper and selects a verified copy of
/// its runtime bundle. The active executable is never run during preparation.
///
/// # Errors
/// Reports missing or modified installations, signing or verification errors,
/// and concurrent activation of a different build.
pub async fn renew_signing(home: &Path, workspace: &Path) -> io::Result<PreparedDevelopment> {
  let workspace = workspace.canonicalize()?;
  let recipe = workspace.join("scripts/dev/ctld-signed.mts");
  if !recipe.is_file() {
    return Err(invalid(
      "choose a ctl checkout containing the macOS helper signer with --workspace",
    ));
  }
  let installations = Installations::new(home);
  let active = installations.active()?.ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::NotFound,
      "no active development installation; first run ctlup dev",
    )
  })?;
  let manifest = &active.manifest;
  if manifest.source != Source::Local {
    return Err(invalid(
      "signing renewal requires an installed local development build",
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
  // --component-info from running, so the recipe probes only the signed result.
  let original = files
    .remove(HELPER)
    .ok_or_else(|| invalid("the active build has no signed development helper"))?;
  files.retain(|name, _| name != "ctld" && name != RECEIPT && !name.starts_with("ctld.app/"));

  let output = OutputDirectory::new()?;
  let artifact = output.0.join("ctld");
  write_private(&artifact, &original, true)?;
  drop(original);
  let helper = output.0.join("helper");
  let input = output.0.join("renewal.json");
  let descriptor = serde_json::to_vec(&SigningInput {
    artifact: &artifact,
    target,
    expected_component: component,
    expected_team: &team,
    output_directory: &helper,
  })
  .map_err(io::Error::other)?;
  if descriptor.len() > MAX_DESCRIPTOR_BYTES {
    return Err(invalid("ctld signing input exceeds its size limit"));
  }
  write_private(&input, &descriptor, false)?;
  let mut command = recipe_command(&recipe, &workspace);
  command.arg("--renew-signing").arg(&input);
  run_recipe(command, "ctld signing renewal").await?;

  let receipt =
    crate::components::read_input(&helper.join("installation.json"), MAX_DESCRIPTOR_BYTES)?;
  if crate::setup::development_signing_team(&receipt, target, component)
    .map_err(io::Error::other)?
    != team
  {
    return Err(invalid("renewed helper uses a different Apple Team"));
  }
  let mut executables = manifest
    .files
    .iter()
    .filter(|(name, info)| info.executable && files.contains_key(*name))
    .map(|(name, _)| name.clone())
    .collect::<Vec<_>>();
  crate::components::collect_app(
    &helper,
    &helper.join("ctld.app"),
    &mut files,
    &mut executables,
  )?;
  let signed = files
    .get(HELPER)
    .ok_or_else(|| invalid("renewal failed to produce a signed helper executable"))?;
  files.insert("ctld".into(), signed.clone());
  files.insert(RECEIPT.into(), receipt);
  let mut replacement =
    Manifest::new(target, manifest.source, manifest.components.clone(), &files)?
      .with_executables(&executables)?;
  if let Some(id) = &manifest.distribution_id {
    replacement = replacement.with_distribution_id(id)?;
  }
  // Verify the exact repackaged bytes in private staging before making the
  // replacement visible to installation readers.
  let bundle = Store::cache(&output.0).publish(&replacement, &files)?;
  drop(files);
  crate::components::verify_local(home, &bundle).await?;

  let (installed, lease) = activate(installations, bundle, active.manifest.bundle_id).await?;
  Ok(prepared_result(home, &installed, lease))
}

async fn activate(
  installations: Installations,
  bundle: Bundle,
  expected: String,
) -> io::Result<(Bundle, Lease)> {
  tokio::task::spawn_blocking(move || {
    let (installed, lease) = installations.activate_renewed_helper(&bundle, &expected)?;
    if installed.manifest.bundle_id != expected
      && let Err(error) = installations.prune()
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
