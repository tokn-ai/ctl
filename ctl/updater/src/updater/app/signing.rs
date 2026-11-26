use super::{
  CachedPackage, DesktopMetadata, DesktopSigning, DesktopTarget, Signing, activation::Workspace,
  invalid, macos, package_digest, report_json,
};
use crate::updater::sign::{self, DesktopInput};
use ctl_client::cli::Channel;
use ctl_client::remote_bundle::BundleSet;
use ctl_core::private_fs;
use std::io::{self, Read as _};
use std::path::Path;

pub(super) async fn sign_if_unsigned(
  work: &Workspace,
  mut package: CachedPackage,
) -> io::Result<CachedPackage> {
  if package.metadata.signing != Signing::Unsigned {
    return Ok(package);
  }
  if package_digest(&package.archive)? != package.metadata.content_id {
    return Err(invalid("unsigned desktop archive checksum mismatch"));
  }
  work.cache(&package)?;
  if let Some(signed) = work.cached_signed_derivative(&package)? {
    return Ok(signed);
  }
  let target = DesktopTarget::native()?;
  let (version, _) = build_identity(&package.metadata)?;
  let Some(archive) =
    sign::sign_for_install(&package.archive, version, target, work.staging()).await?
  else {
    eprintln!(
      "Continuing with unsigned ctmux. Automatic signing needs a Developer ID Application identity and Apple signing credentials."
    );
    return Ok(package);
  };
  let digest = package_digest(&archive)?;
  package.metadata.signed_from = Some(std::mem::replace(&mut package.metadata.content_id, digest));
  package.metadata.signing = Signing::Signed;
  package.archive = archive;
  Ok(package)
}

pub(in crate::updater) async fn sign_cached(
  home: &Path,
  channel: Option<Channel>,
  version: Option<&str>,
  target: DesktopTarget,
  json: bool,
) -> io::Result<()> {
  let work = Workspace::begin(home)?;
  let mut package = work.signing_package(channel.map(Into::into), version, target)?;
  if package.metadata.signing == Signing::Signed {
    if json {
      return report_json(&serde_json::json!({
        "status": "already_signed",
        "bundle_id": package.metadata.bundle_id,
        "target": package.metadata.target_triple,
      }));
    }
    println!("The cached desktop build is already signed.");
    return Ok(());
  }
  if package.metadata.signing != Signing::Unsigned {
    return Err(invalid("Choose a verified cached CI desktop build to sign"));
  }
  let snapshot = work.staging().join("signing-input");
  super::copy_package(&package.archive, &snapshot)?;
  if package_digest(&snapshot)? != package.metadata.content_id {
    return Err(invalid(
      "cached desktop archive changed during signing preparation",
    ));
  }
  package.archive = snapshot;
  let revision = match &package.metadata.provenance {
    Some(provenance) => provenance.source_revision.clone(),
    None => legacy_revision(work.staging(), &package)?,
  };
  sign::export_desktop(home, desktop_input(&package, target, &revision)?, json).await
}

fn desktop_input<'a>(
  package: &'a CachedPackage,
  target: DesktopTarget,
  source_revision: &'a str,
) -> io::Result<DesktopInput<'a>> {
  let (app_version, bundle_id) = build_identity(&package.metadata)?;
  Ok(DesktopInput {
    archive: &package.archive,
    app_version,
    bundle_id,
    source_revision,
    installation_layout_version: package.naming.installation_layout_version,
    target,
  })
}

fn build_identity(metadata: &DesktopMetadata) -> io::Result<(&str, &str)> {
  metadata
    .app_version
    .as_deref()
    .zip(metadata.bundle_id.as_deref())
    .ok_or_else(|| invalid("cached desktop metadata has no build identity"))
}

fn legacy_revision(staging: &Path, package: &CachedPackage) -> io::Result<String> {
  let (app_version, bundle_id) = build_identity(&package.metadata)?;
  let check = staging.join("inspect-legacy");
  private_fs::create_directory(&check)?;
  let app = macos::stage(
    &package.archive,
    &check,
    app_version,
    DesktopSigning::Unsigned,
  )?;
  let receipt = app.join("Contents/Resources/resources/agent-bundles/bundle-set.json");
  let mut bytes = Vec::new();
  let file = private_fs::options().read(true).open(receipt)?;
  if !file.metadata()?.is_file() {
    return Err(invalid(
      "missing embedded bundle set for the legacy desktop archive",
    ));
  }
  file.take(65537).read_to_end(&mut bytes)?;
  let set = BundleSet::parse(&bytes, app_version).map_err(io::Error::other)?;
  if set.bundle_id != bundle_id {
    return Err(invalid(
      "legacy desktop bundle identity differs from the embedded bundle set",
    ));
  }
  std::fs::remove_dir_all(check)?;
  Ok(set.git_revision)
}
