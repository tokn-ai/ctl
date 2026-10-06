//! Explicit complete-bundle import, selection and packaging for either purpose.

use ctl_core::bundles::{
  Bundle, COMPONENTS, MANIFEST_FILE, MAX_FILE_BYTES, Manifest, Purpose, Source, Store,
};
use ctl_core::component::ComponentInfo;
use std::collections::BTreeMap;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

/// Imports a verified CI/release bundle without changing selection or services.
///
/// # Errors
/// Rejects incompatible, incomplete, modified or schema-1 bundles.
pub fn import_remote(
  home: &Path,
  bundle: &crate::remote_bundle::VerifiedBundle,
  target: &str,
  source: Source,
) -> Result<Bundle, crate::remote_bundle::Error> {
  let payload = remote_payload(bundle, target, source)?;
  Ok(Store::new(home).publish(&payload.manifest, &payload.files)?)
}

/// Inspects a complete archive without importing, selecting or executing it.
/// Its content identity is the same one used by a later explicit import.
///
/// # Errors
/// Rejects incompatible, incomplete, modified or schema-1 bundles.
pub fn inspect_remote(
  bundle: &crate::remote_bundle::VerifiedBundle,
  target: &str,
  source: Source,
) -> Result<Manifest, crate::remote_bundle::Error> {
  remote_payload(bundle, target, source).map(|payload| payload.manifest)
}

struct RemotePayload {
  manifest: Manifest,
  files: BTreeMap<String, Vec<u8>>,
}

fn remote_payload(
  bundle: &crate::remote_bundle::VerifiedBundle,
  target: &str,
  source: Source,
) -> Result<RemotePayload, crate::remote_bundle::Error> {
  let outer = crate::remote_bundle::BundleSet::parse_intrinsic(&bundle.manifest)?;
  if !outer.is_compatible(target)? {
    return Err(crate::remote_bundle::Error::NotAvailable(
      "bundle does not advertise the required component contracts".into(),
    ));
  }
  outer.verify_archive_bytes(target, &bundle.archive)?;
  crate::remote_bundle::compatibility::verify_archive(&outer, target, &bundle.archive)?;
  let components = outer
    .target(target)?
    .components
    .clone()
    .ok_or_else(|| invalid("bundle lacks component advertisements"))?;
  let mut files = BTreeMap::new();
  let mut archive = tar::Archive::new(flate2::read::MultiGzDecoder::new(bundle.archive.as_slice()));
  for entry in archive.entries()? {
    let mut entry = entry?;
    let name = entry
      .path()?
      .to_str()
      .ok_or_else(|| invalid("non-UTF8 bundle path"))?
      .to_owned();
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    files.insert(name, bytes);
  }
  let manifest =
    Manifest::new(target, source, components, &files)?.with_distribution_id(&bundle.bundle_id)?;
  Ok(RemotePayload { manifest, files })
}

/// Snapshots an explicitly chosen native local build. The complete signed macOS
/// helper package is verified before querying its metadata and retained intact.
///
/// # Errors
/// Rejects mismatched builds, unsafe files or an invalid signed helper.
pub async fn import_local(
  home: &Path,
  directory: &Path,
  helper: Option<&Path>,
) -> io::Result<Bundle> {
  let mut components = BTreeMap::new();
  let mut files = BTreeMap::new();
  let mut app_executables = Vec::new();
  #[cfg(target_os = "macos")]
  let signed_helper = if let Some(helper) = helper {
    let receipt = read_input(
      &helper.join("installation.json"),
      ctl_core::bundles::MAX_MANIFEST_BYTES,
    )?;
    let info = crate::setup::inspect_ctld_package(home, helper, &receipt)
      .await
      .map_err(io::Error::other)?;
    collect_app(
      helper,
      &helper.join("ctld.app"),
      &mut files,
      &mut app_executables,
    )?;
    files.insert("ctld-package.json".into(), receipt);
    Some((helper.join("ctld.app/Contents/MacOS/ctld"), info))
  } else {
    None
  };
  #[cfg(not(target_os = "macos"))]
  let signed_helper: Option<(std::path::PathBuf, ComponentInfo)> = {
    let _ = (helper, &mut app_executables);
    None
  };
  for name in COMPONENTS {
    let path = if name == "ctld" {
      signed_helper
        .as_ref()
        .map_or_else(|| directory.join(name), |(path, _)| path.clone())
    } else {
      directory.join(name)
    };
    let bytes = read_input(&path, MAX_FILE_BYTES)?;
    let info = if name == "ctld" {
      match &signed_helper {
        Some((_, info)) => info.clone(),
        None => ctl_core::executable::inspect(&path).await?,
      }
    } else {
      ctl_core::executable::inspect(&path).await?
    };
    if read_input(&path, MAX_FILE_BYTES)? != bytes {
      return Err(invalid("local component changed while being imported"));
    }
    check_total(&files, bytes.len())?;
    components.insert(name.into(), info);
    files.insert(name.into(), bytes);
  }
  if !compatible(&components) {
    return Err(invalid("local build is incompatible with this client"));
  }
  let home = home.to_owned();
  tokio::task::spawn_blocking(move || publish_local(&home, components, files, &app_executables))
    .await
    .map_err(io::Error::other)?
}

fn publish_local(
  home: &Path,
  components: BTreeMap<String, ComponentInfo>,
  mut files: BTreeMap<String, Vec<u8>>,
  app_executables: &[String],
) -> io::Result<Bundle> {
  let target = ctl_core::paths::native_target();
  let manifest =
    Manifest::new(target, Source::Local, components, &files)?.with_executables(app_executables)?;
  // Retain a publisher identity for older compatible agents that only read the
  // legacy provenance file. The complete manifest still binds every byte and
  // every advertised contract; legacy metadata makes no extra protocol claims.
  let publisher_id = manifest.bundle_id.clone();
  let build = &manifest.components["ctl-agent"].build;
  files.insert(
    "manifest.json".into(),
    serde_json::to_vec(&serde_json::json!({
      "schema_version": 1, "app_version": build.version, "bundle_id": publisher_id,
      "git_revision": build.source_revision, "target_triple": target,
    }))
    .map_err(io::Error::other)?,
  );
  let manifest = Manifest::new(target, Source::Local, manifest.components, &files)?
    .with_executables(app_executables)?
    .with_distribution_id(&publisher_id)?;
  Store::new(home).publish(&manifest, &files)
}

/// Selects an imported complete build, without starting or restarting services.
///
/// # Errors
/// Rejects changed bytes, incompatible contracts or invalid native signing.
pub async fn select(home: &Path, purpose: Purpose, bundle: &Bundle) -> io::Result<()> {
  select_with_progress(home, purpose, bundle, || {}).await
}

/// Selects a complete bundle, notifying the caller after verification finishes.
///
/// # Errors
/// Rejects changed bytes, incompatible contracts or invalid native signing.
pub async fn select_with_progress(
  home: &Path,
  purpose: Purpose,
  bundle: &Bundle,
  verified: impl FnOnce(),
) -> io::Result<()> {
  if purpose == Purpose::Upload && !upload_target(&bundle.manifest.target_triple) {
    return Err(invalid("this target is unavailable for remote uploads"));
  }
  if !compatible(&bundle.manifest.components) {
    return Err(invalid(
      "bundle is incompatible with this client; choose a compatible complete build",
    ));
  }
  #[cfg(target_os = "macos")]
  if purpose == Purpose::Local {
    let files = bundle.read_files()?;
    let receipt = files
      .get("ctld-package.json")
      .ok_or_else(|| invalid("local macOS bundle lacks its signed helper receipt"))?;
    let info = crate::setup::inspect_ctld_package(home, &bundle.directory, receipt)
      .await
      .map_err(io::Error::other)?;
    if !bundle.manifest.same_component("ctld", &info) {
      return Err(invalid(
        "signed helper differs from the selected complete build",
      ));
    }
  }
  verified();
  let home = home.to_owned();
  let bundle = bundle.clone();
  tokio::task::spawn_blocking(move || Store::new(&home).select(purpose, &bundle))
    .await
    .map_err(io::Error::other)?
}

/// Portable targets recognized by the remote platform probe.
#[must_use]
pub fn upload_target(target: &str) -> bool {
  matches!(
    target,
    "x86_64-unknown-linux-musl"
      | "aarch64-unknown-linux-musl"
      | "x86_64-apple-darwin"
      | "aarch64-apple-darwin"
  )
}

/// Initializes the first upload selection without replacing an explicit choice.
///
/// # Errors
/// Rejects incompatible candidates or an unsafe existing selection.
pub async fn initialize_upload(home: &Path, bundle: &Bundle) -> io::Result<Bundle> {
  if !upload_target(&bundle.manifest.target_triple) || !compatible(&bundle.manifest.components) {
    return Err(invalid(
      "initial upload bundle is incompatible with this client",
    ));
  }
  let home = home.to_owned();
  let bundle = bundle.clone();
  tokio::task::spawn_blocking(move || Store::new(&home).select_if_unset(Purpose::Upload, &bundle))
    .await
    .map_err(io::Error::other)?
}

/// Packages exactly the selected immutable complete build for remote upload.
///
/// # Errors
/// Rejects changed bytes, incompatible contracts or an oversized archive.
pub fn upload_bundle(bundle: &Bundle) -> io::Result<crate::remote_bundle::VerifiedBundle> {
  if !upload_target(&bundle.manifest.target_triple) {
    return Err(invalid(
      "selected upload bundle is incompatible with this client",
    ));
  }
  package_bundle(bundle)
}

pub(crate) fn package_bundle(bundle: &Bundle) -> io::Result<crate::remote_bundle::VerifiedBundle> {
  if !compatible(&bundle.manifest.components) {
    return Err(invalid("bundle is incompatible with this client"));
  }
  let files = bundle.read_files()?;
  let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
  let mut archive = tar::Builder::new(encoder);
  for (name, bytes) in &files {
    append(
      &mut archive,
      name,
      bytes,
      bundle.manifest.files[name].executable,
    )?;
  }
  let manifest = serde_json::to_vec(&bundle.manifest).map_err(io::Error::other)?;
  append(&mut archive, MANIFEST_FILE, &manifest, false)?;
  let archive = archive.into_inner()?.finish()?;
  if archive.len() > MAX_FILE_BYTES {
    return Err(invalid("upload archive exceeds its size limit"));
  }
  let build = &bundle.manifest.components["ctl-agent"].build;
  Ok(crate::remote_bundle::VerifiedBundle {
    app_version: build.version.clone(),
    bundle_id: bundle
      .manifest
      .distribution_id
      .clone()
      .unwrap_or_else(|| bundle.manifest.bundle_id.clone()),
    git_revision: build
      .source_revision
      .clone()
      .ok_or_else(|| invalid("bundle has no source revision"))?,
    file_name: format!(
      "ctl-components-{}-{}.tar.gz",
      bundle.manifest.bundle_id, bundle.manifest.target_triple
    ),
    archive,
    manifest,
  })
}

#[must_use]
pub fn compatible(components: &BTreeMap<String, ComponentInfo>) -> bool {
  crate::remote_bundle::compatibility::compatible(components)
}

/// Validates the complete transport archive before building a remote script.
/// Legacy archives remain accepted by their existing caller-side verifier.
///
/// # Errors
/// Rejects unsafe, excessive or checksum-invalid managed payloads.
pub(crate) fn inspect_upload_archive(bytes: &[u8]) -> io::Result<Option<Manifest>> {
  let mut files = read_archive(bytes)?;
  let Some(bytes) = files.remove(MANIFEST_FILE) else {
    return Ok(None);
  };
  if bytes.len() > ctl_core::bundles::MAX_MANIFEST_BYTES {
    return Err(invalid("component manifest exceeds its size limit"));
  }
  let manifest: Manifest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  manifest.validate()?;
  manifest.verify_files(&files)?;
  Ok(Some(manifest))
}

pub(crate) fn read_archive(bytes: &[u8]) -> io::Result<BTreeMap<String, Vec<u8>>> {
  if bytes.len() > MAX_FILE_BYTES {
    return Err(invalid("component archive exceeds its size limit"));
  }
  let decoder = flate2::read::MultiGzDecoder::new(bytes)
    .take(ctl_core::bundles::MAX_TOTAL_BYTES as u64 + 1024 * 1024);
  let mut archive = tar::Archive::new(decoder);
  let mut files = BTreeMap::new();
  let mut total = 0usize;
  for entry in archive.entries()?.raw(true) {
    let mut entry = entry?;
    let name = std::str::from_utf8(&entry.path_bytes())
      .map_err(io::Error::other)?
      .to_owned();
    if entry.header().entry_type() != tar::EntryType::Regular
      || entry.size() == 0
      || entry.size() > MAX_FILE_BYTES as u64
      || files.len() > 512
    {
      return Err(invalid("unsafe or excessive component archive entries"));
    }
    let mut payload = Vec::new();
    entry.read_to_end(&mut payload)?;
    total = total.saturating_add(payload.len());
    if total > ctl_core::bundles::MAX_TOTAL_BYTES + ctl_core::bundles::MAX_MANIFEST_BYTES {
      return Err(invalid("component archive exceeds its payload limit"));
    }
    if name.contains(['\\', '\0'])
      || name
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
      return Err(invalid("unsafe component archive path"));
    }
    if files.insert(name, payload).is_some() {
      return Err(invalid("duplicate component archive entry"));
    }
  }
  let mut padding = archive.into_inner();
  let mut buffer = [0; 8192];
  loop {
    let read = padding.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    if buffer[..read].iter().any(|byte| *byte != 0) {
      return Err(invalid("component archive contains trailing data"));
    }
  }
  if padding.limit() == 0 {
    return Err(invalid("component archive exceeds its expanded limit"));
  }
  Ok(files)
}

pub(crate) fn append<W: io::Write>(
  archive: &mut tar::Builder<W>,
  name: &str,
  bytes: &[u8],
  executable: bool,
) -> io::Result<()> {
  let mut header = tar::Header::new_ustar();
  header.set_size(bytes.len() as u64);
  header.set_mode(if executable { 0o700 } else { 0o600 });
  header.set_cksum();
  archive.append_data(&mut header, name, bytes)
}

pub(crate) fn read_input(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
  let metadata = std::fs::symlink_metadata(path)?;
  if !metadata.is_file() || metadata.mode() & 0o022 != 0 {
    return Err(invalid(
      "component input must be a regular file, not a link or writable by another account",
    ));
  }
  let file = std::fs::File::from(
    rustix::fs::open(
      path,
      rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC,
      rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)?,
  );
  let metadata = file.metadata()?;
  if !metadata.is_file() || metadata.mode() & 0o022 != 0 {
    return Err(invalid("component input changed to an unsafe file"));
  }
  let mut bytes = Vec::new();
  file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
  if bytes.len() > maximum {
    return Err(invalid("component input exceeds its size limit"));
  }
  Ok(bytes)
}

#[cfg(target_os = "macos")]
fn collect_app(
  root: &Path,
  directory: &Path,
  files: &mut BTreeMap<String, Vec<u8>>,
  executables: &mut Vec<String>,
) -> io::Result<()> {
  if !std::fs::symlink_metadata(directory)?.is_dir() {
    return Err(invalid("helper package contains an unsafe directory"));
  }
  for entry in std::fs::read_dir(directory)? {
    let entry = entry?;
    let path = entry.path();
    let kind = entry.file_type()?;
    if kind.is_dir() {
      collect_app(root, &path, files, executables)?;
    } else if kind.is_file() {
      if files.len() >= 512 {
        return Err(invalid("helper app contains too many files"));
      }
      let name = path
        .strip_prefix(root)
        .map_err(io::Error::other)?
        .to_str()
        .ok_or_else(|| invalid("non-UTF8 helper path"))?
        .to_owned();
      if entry.metadata()?.mode() & 0o111 != 0 {
        executables.push(name.clone());
      }
      let bytes = read_input(&path, MAX_FILE_BYTES)?;
      check_total(files, bytes.len())?;
      files.insert(name, bytes);
    } else {
      return Err(invalid("helper app contains links or special files"));
    }
  }
  Ok(())
}

fn check_total(files: &BTreeMap<String, Vec<u8>>, added: usize) -> io::Result<()> {
  let total: usize = files.values().map(Vec::len).sum();
  if total.saturating_add(added) > ctl_core::bundles::MAX_TOTAL_BYTES {
    return Err(invalid("component import exceeds its total size limit"));
  }
  Ok(())
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
