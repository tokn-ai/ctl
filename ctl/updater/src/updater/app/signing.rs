use super::{
  CachedPackage, DesktopMetadata, DesktopSigning, DesktopTarget, Signing, activation::Workspace,
  invalid, macos, package_digest, report_json,
};
use crate::updater::sign::{self, DesktopInput, DesktopSigningOptions, SigningMode};
use ctl_client::cli::Channel;
use ctl_client::remote_bundle::BundleSet;
use ctl_core::private_fs;
use std::io::{self, Read as _};
use std::path::Path;

impl From<SigningMode> for Signing {
  fn from(mode: SigningMode) -> Self {
    match mode {
      SigningMode::DeveloperId => Self::Signed,
      SigningMode::Provision => Self::Provisioned,
      SigningMode::AdHoc => Self::AdHoc,
    }
  }
}

pub(super) async fn prepare_signing(
  home: &Path,
  work: &Workspace,
  mut package: CachedPackage,
  mode: SigningMode,
  interactive: bool,
) -> io::Result<CachedPackage> {
  if package.metadata.signing == Signing::Provided {
    let staging = inspection_directory(work)?;
    let (app, version) = macos::stage_for_signing(&package.archive, &staging)?;
    package.metadata.signing = if macos::verify_provided_distribution(&app, &version)? {
      Signing::Signed
    } else {
      Signing::Unsigned
    };
    package
      .metadata
      .bundle_id
      .get_or_insert_with(|| version.clone());
    package.metadata.app_version = Some(version);
  }
  if package.metadata.signing == Signing::Signed {
    package.metadata.signing_mode = Some(mode);
    return Ok(package);
  }
  let profile = if mode == SigningMode::Provision && sign::has_profile_override() {
    Some(sign::select_provisioning(home, false, interactive).await?)
  } else {
    None
  };
  let team = match &profile {
    Some(profile) => Some(profile.team.clone()),
    None if mode == SigningMode::Provision => sign::preferred_team(home)?,
    None => None,
  };
  if let Some(source) = &package.metadata.signed_from {
    if package.metadata.signing == Signing::from(mode)
      && verified_for_reuse(work, &package, mode, team.as_deref())?
    {
      return Ok(package);
    }
    package = work.cached(source)?.ok_or_else(|| {
      invalid("the unsigned desktop source is missing; reinstall from the original source")
    })?;
  }
  if package.metadata.signing != Signing::Unsigned {
    return Err(invalid("expected an unsigned macOS desktop archive"));
  }
  if package_digest(&package.archive)? != package.metadata.content_id {
    return Err(invalid("unsigned desktop archive checksum mismatch"));
  }
  work.cache(&package)?;
  if let Some(mut signed) = work.cached_signed_derivative(&package, mode)?
    && verified_for_reuse(work, &signed, mode, team.as_deref())?
  {
    signed.naming = package.naming;
    signed.metadata.signing_mode = Some(mode);
    return Ok(signed);
  }
  let target = DesktopTarget::native()?;
  let (version, _) = build_identity(&package.metadata)?;
  let archive = sign::sign_for_install(
    home,
    &package.archive,
    version,
    target,
    work.staging(),
    DesktopSigningOptions {
      mode,
      interactive,
      profile: profile.as_ref(),
    },
  )
  .await?;
  let digest = package_digest(&archive)?;
  package.metadata.signed_from = Some(std::mem::replace(&mut package.metadata.content_id, digest));
  package.metadata.signing = mode.into();
  package.metadata.signing_mode = Some(mode);
  package.archive = archive;
  Ok(package)
}

fn inspection_directory(work: &Workspace) -> io::Result<std::path::PathBuf> {
  let directory = work
    .staging()
    .join(format!("signing-check-{}", uuid::Uuid::new_v4()));
  private_fs::create_directory(&directory)?;
  Ok(directory)
}

fn verified_for_reuse(
  work: &Workspace,
  package: &CachedPackage,
  mode: SigningMode,
  team: Option<&str>,
) -> io::Result<bool> {
  if mode == SigningMode::DeveloperId {
    return Ok(true);
  }
  let (version, _) = build_identity(&package.metadata)?;
  match macos::stage_local(
    &package.archive,
    &inspection_directory(work)?,
    version,
    mode,
  ) {
    Ok((_, signed_team)) => Ok(team.is_none_or(|team| signed_team.as_deref() == Some(team))),
    Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(false),
    Err(error) => Err(error),
  }
}

pub(in crate::updater) async fn sign_cached(
  home: &Path,
  channel: Option<Channel>,
  version: Option<&str>,
  target: DesktopTarget,
  mode: Option<SigningMode>,
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
  let mode = mode.or(package.metadata.signing_mode).unwrap_or_default();
  if mode != SigningMode::DeveloperId {
    let signed = prepare_signing(home, &work, package, mode, !json).await?;
    work.cache(&signed)?;
    let cached = work
      .cached(&signed.metadata.content_id)?
      .ok_or_else(|| invalid("signed desktop archive is missing from the cache"))?;
    if json {
      return report_json(&serde_json::json!({
        "status": "signed",
        "signing": signed.metadata.signing,
        "bundle_id": signed.metadata.bundle_id,
        "target": signed.metadata.target_triple,
        "archive": cached.archive,
      }));
    }
    println!(
      "Signed the cached desktop build at {}.",
      cached.archive.display()
    );
    return Ok(());
  }
  if let Some(source) = &package.metadata.signed_from {
    package = work
      .cached(source)?
      .ok_or_else(|| invalid("the unsigned desktop source is missing"))?;
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
