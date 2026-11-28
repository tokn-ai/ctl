//! Build and verify native runtime derivatives without changing their publication.

use super::{append, collect_app, invalid, read_input, verify_local_for_activation};
use crate::component_update::PreparedUpdate;
use ctl_core::bundles::{
  Bundle, LEGACY_MANIFEST_FILE, MACOS_RUNTIME_SIGNING_FIELD, MAX_FILE_BYTES, Manifest, Store,
  macos_ad_hoc_receipt,
};
use ctl_core::component::ComponentInfo;
use ctl_core::executable::PreparedExecutable;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const SIGNED_RECEIPT: &str = "ctld-package.json";
const SIGNED_EXECUTABLE: &str = "ctld.app/Contents/MacOS/ctld";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacosRuntimeHelperMode {
  DeveloperId,
  Provision,
  AdHoc,
}

/// An explicitly selected replacement signed in a private staging directory.
pub enum MacosRuntimeHelper<'a> {
  AdHoc {
    executable: &'a Path,
  },
  Signed {
    app: &'a Path,
    team_identifier: &'a str,
    development: bool,
  },
}

/// Inspects the receipt's declared mode. Local activation separately verifies
/// the matching executable's signature, component identity, and entitlements.
///
/// # Errors
/// Rejects invalid receipts, incompatible target metadata, or changed files.
pub fn macos_runtime_helper_mode(bundle: &Bundle) -> io::Result<Option<MacosRuntimeHelperMode>> {
  if !bundle.manifest.target_triple.ends_with("-apple-darwin") {
    return Err(invalid("runtime signing requires a macOS target"));
  }
  helper_mode(&bundle.read_files()?, &bundle.manifest.target_triple)
}

fn helper_mode(
  files: &BTreeMap<String, Vec<u8>>,
  target: &str,
) -> io::Result<Option<MacosRuntimeHelperMode>> {
  let signed = files.get(SIGNED_RECEIPT);
  let packaged_app = files.keys().any(|name| name.starts_with("ctld.app/"));
  if macos_ad_hoc_receipt(files)? {
    if signed.is_some() || packaged_app {
      return Err(invalid(
        "invalid local ad hoc runtime receipt or helper layout",
      ));
    }
    return Ok(Some(MacosRuntimeHelperMode::AdHoc));
  }
  match signed {
    Some(receipt) => {
      if !files.contains_key(SIGNED_EXECUTABLE) {
        return Err(invalid(
          "runtime signing receipt has no matching helper app",
        ));
      }
      let development =
        crate::setup::is_development_helper_receipt(receipt, target).map_err(io::Error::other)?;
      Ok(Some(if development {
        MacosRuntimeHelperMode::Provision
      } else {
        MacosRuntimeHelperMode::DeveloperId
      }))
    }
    None if packaged_app => Err(invalid(
      "local macOS bundle lacks its signed helper receipt",
    )),
    None => Ok(None),
  }
}

/// Authorizes an explicit ad hoc selection from an existing prepared cache.
///
/// # Errors
/// Rejects a cache entry without a valid ad hoc receipt.
pub fn authorize_macos_ad_hoc(prepared: &mut PreparedUpdate) -> io::Result<()> {
  if macos_runtime_helper_mode(&prepared.bundle)? != Some(MacosRuntimeHelperMode::AdHoc) {
    return Err(invalid("the selected runtime has no local ad hoc receipt"));
  }
  prepared.authorized_ad_hoc_bundle = Some(prepared.bundle.manifest.bundle_id.clone());
  prepared.require_macos_runtime_signing();
  Ok(())
}

/// Stages a signed derivative of the selected runtime and keeps its exact
/// verified bytes available for pinned recovery. The publisher identity,
/// updater selection and catalog snapshot are retained from preparation.
///
/// # Errors
/// Rejects foreign targets, malformed receipts, mismatched components,
/// failed signature checks, or modification of a signed package.
pub async fn replace_macos_helper(
  home: &Path,
  prepared: &mut PreparedUpdate,
  staging: &Path,
  helper: MacosRuntimeHelper<'_>,
) -> io::Result<()> {
  if prepared.bundle.manifest.target_triple != ctl_core::paths::native_target() {
    return Err(invalid("runtime signing requires this Mac's native target"));
  }
  let original = &prepared.bundle.manifest;
  let mut files = prepared.bundle.read_files()?;
  let original_mode = helper_mode(&files, &original.target_triple)?;
  if matches!(
    original_mode,
    Some(MacosRuntimeHelperMode::DeveloperId | MacosRuntimeHelperMode::Provision)
  ) {
    return Err(invalid(
      "the selected runtime already claims a signed helper",
    ));
  }
  let mut executables = original
    .files
    .iter()
    .filter(|(_, info)| info.executable)
    .map(|(name, _)| name.clone())
    .collect::<Vec<_>>();
  let ad_hoc = matches!(helper, MacosRuntimeHelper::AdHoc { .. });
  if ad_hoc || original_mode == Some(MacosRuntimeHelperMode::AdHoc) {
    set_ad_hoc_receipt(original, &mut files, ad_hoc)?;
  }
  match helper {
    MacosRuntimeHelper::AdHoc { executable } => {
      files.insert("ctld".into(), read_input(executable, MAX_FILE_BYTES)?);
    }
    MacosRuntimeHelper::Signed {
      app,
      team_identifier,
      development,
    } => {
      let parent = app
        .parent()
        .filter(|_| app.file_name().is_some_and(|name| name == "ctld.app"))
        .ok_or_else(|| invalid("expected a staged ctld.app package"))?;
      let mut signed = BTreeMap::new();
      collect_app(parent, app, &mut signed, &mut executables)?;
      let executable = signed
        .get(SIGNED_EXECUTABLE)
        .ok_or_else(|| invalid("runtime signing did not produce a ctld executable"))?;
      let archive = helper_archive(&signed, &executables)?;
      let receipt = crate::setup::create_runtime_helper_receipt(
        &original.components["ctld"],
        &original.target_triple,
        team_identifier,
        development,
        hex::encode(Sha256::digest(&archive)),
        archive.len() as u64,
      )
      .map_err(io::Error::other)?;
      files.insert("ctld".into(), executable.clone());
      files.insert(SIGNED_RECEIPT.into(), receipt);
      files.append(&mut signed);
    }
  }
  let mut manifest = Manifest::new(
    &original.target_triple,
    original.source,
    original.components.clone(),
    &files,
  )?
  .with_executables(&executables)?;
  if let Some(id) = &original.distribution_id {
    manifest = manifest.with_distribution_id(id)?;
  }
  let candidate = Bundle::stage(staging, &manifest, &files)?;
  verify_local_for_activation(home, &candidate, ad_hoc).await?;
  prepared.bundle = Store::cache(home).publish(&manifest, &files)?;
  prepared.authorized_ad_hoc_bundle = ad_hoc.then_some(manifest.bundle_id);
  prepared.require_macos_runtime_signing();
  Ok(())
}

fn set_ad_hoc_receipt(
  original: &Manifest,
  files: &mut BTreeMap<String, Vec<u8>>,
  ad_hoc: bool,
) -> io::Result<()> {
  // Preserve the legacy record older agents know and keep the opt-in bound to
  // the managed manifest without introducing a new payload file name.
  let mut metadata = if let Some(bytes) = files.get(LEGACY_MANIFEST_FILE) {
    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(bytes)
      .map_err(io::Error::other)?
  } else {
    let build = &original.components["ctl-agent"].build;
    serde_json::from_value(serde_json::json!({
      "schema_version": 1,
      "app_version": build.version,
      "bundle_id": original.distribution_id.as_ref().unwrap_or(&original.bundle_id),
      "git_revision": build.source_revision,
      "target_triple": original.target_triple,
    }))
    .map_err(io::Error::other)?
  };
  if ad_hoc {
    metadata.insert(MACOS_RUNTIME_SIGNING_FIELD.into(), "ad_hoc".into());
  } else {
    metadata.remove(MACOS_RUNTIME_SIGNING_FIELD);
  }
  files.insert(
    LEGACY_MANIFEST_FILE.into(),
    serde_json::to_vec(&metadata).map_err(io::Error::other)?,
  );
  Ok(())
}

fn helper_archive(
  files: &BTreeMap<String, Vec<u8>>,
  executables: &[String],
) -> io::Result<Vec<u8>> {
  let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
  let mut archive = tar::Builder::new(encoder);
  for (name, bytes) in files {
    append(&mut archive, name, bytes, executables.contains(name))?;
  }
  archive.into_inner()?.finish()
}

pub(crate) async fn verify_ad_hoc(
  bundle: &Bundle,
  files: &BTreeMap<String, Vec<u8>>,
) -> io::Result<ComponentInfo> {
  if bundle.manifest.target_triple != ctl_core::paths::native_target()
    || helper_mode(files, &bundle.manifest.target_triple)? != Some(MacosRuntimeHelperMode::AdHoc)
  {
    return Err(invalid(
      "expected an explicitly ad hoc signed native runtime",
    ));
  }
  let executable = bundle.directory.join("ctld");
  let display = tokio::time::timeout(
    Duration::from_secs(10),
    Command::new("/usr/bin/codesign")
      .args(["--display", "--verbose=2"])
      .arg(&executable)
      .stdin(Stdio::null())
      .kill_on_drop(true)
      .output(),
  )
  .await??;
  if !display.status.success()
    || !String::from_utf8_lossy(&display.stderr)
      .lines()
      .any(|line| line == "Signature=adhoc")
  {
    return Err(invalid("local ctld does not have an ad hoc signature"));
  }
  let status = tokio::time::timeout(
    Duration::from_secs(10),
    Command::new("/usr/bin/codesign")
      .args(["--verify", "--strict", "--all-architectures"])
      .arg(&executable)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .status(),
  )
  .await??;
  if !status.success() {
    return Err(invalid("local ctld's ad hoc signature failed verification"));
  }
  let prepared = PreparedExecutable::prepare(executable, &[]).await?;
  if !bundle.manifest.same_component("ctld", &prepared.info)
    || read_input(&prepared.path, MAX_FILE_BYTES)? != files["ctld"]
  {
    return Err(invalid(
      "ad hoc signed ctld differs from the selected complete build",
    ));
  }
  prepared.verify().await?;
  Ok(prepared.info)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_ad_hoc_receipt_must_be_explicit_and_exclusive() {
    let target = "aarch64-apple-darwin";
    let mut files = BTreeMap::new();
    assert_eq!(helper_mode(&files, target).unwrap(), None);
    files.insert(
      LEGACY_MANIFEST_FILE.into(),
      br#"{"schema_version":1,"local_macos_signing":"ad_hoc"}"#.to_vec(),
    );
    assert_eq!(
      helper_mode(&files, target).unwrap(),
      Some(MacosRuntimeHelperMode::AdHoc)
    );
    files.insert(SIGNED_EXECUTABLE.into(), vec![1]);
    assert!(helper_mode(&files, target).is_err());
    files.remove(LEGACY_MANIFEST_FILE);
    assert!(helper_mode(&files, target).is_err());
    files.remove(SIGNED_EXECUTABLE);
    files.insert(
      LEGACY_MANIFEST_FILE.into(),
      br#"{"local_macos_signing":"signed"}"#.to_vec(),
    );
    assert!(helper_mode(&files, target).is_err());
  }
}
