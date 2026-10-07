//! Shared package/source policy for explicit app and CLI component updates.

use ctl_core::bundles::agent::{AgentSource, SOURCE_FILE};
use ctl_core::bundles::{Bundle, MANIFEST_FILE, MAX_FILE_BYTES, Manifest, Purpose, Source, Store};
use std::io;
use std::path::{Path, PathBuf};

pub use ctl_core::component_update::{BuildSource, Package, UpdateOptions, UpdateResult};

/// Resolves an explicit source or the pinned selection for a target. A missing
/// initial selection can use an included build, but never replaces a selection.
///
/// # Errors
/// Rejects wrong-target, incomplete, corrupt or incompatible builds.
pub async fn prepare(
  home: &Path,
  target: &str,
  purpose: Purpose,
  source: &BuildSource,
  included: &[PathBuf],
) -> io::Result<Bundle> {
  if let BuildSource::Provided {
    path,
    local_build: true,
    ctld_package,
  } = source
  {
    if target != ctl_core::paths::native_target() {
      return Err(invalid(
        "native build files cannot be uploaded to another target",
      ));
    }
    return crate::components::import_local(home, path, ctld_package.as_deref()).await;
  }
  let home = home.to_owned();
  let target = target.to_owned();
  let source = source.clone();
  let included = included.to_vec();
  tokio::task::spawn_blocking(move || {
    let store = Store::new(&home);
    let bundle = match source {
      BuildSource::Selected => {
        if let Some(bundle) = store.selected(purpose, &target)? {
          bundle
        } else {
          let bundle = included_build(&home, &included, &target, purpose)?;
          if purpose == Purpose::Upload {
            store.select_if_unset(purpose, &bundle)?
          } else {
            bundle
          }
        }
      }
      BuildSource::Provided { path, .. } => provided_build(&home, &path, &target, purpose)?,
    };
    verify_source(&bundle.manifest, &target, purpose)?;
    Ok(bundle)
  })
  .await
  .map_err(io::Error::other)?
}

fn verify_source(manifest: &Manifest, target: &str, purpose: Purpose) -> io::Result<()> {
  let matching_target = if purpose == Purpose::Local {
    ctl_core::bundles::local_target(target)
      && ctl_core::bundles::local_target(&manifest.target_triple)
  } else {
    manifest.target_triple == target
  };
  if !matching_target || !crate::components::compatible(&manifest.components) {
    return Err(invalid(
      "choose a compatible complete build for this host's target",
    ));
  }
  Ok(())
}

fn publisher_target(target: &str, purpose: Purpose) -> std::borrow::Cow<'_, str> {
  // Publisher bundles provide portable musl artifacts for GNU Linux clients.
  if purpose == Purpose::Local
    && let Some(arch) = target.strip_suffix("-unknown-linux-gnu")
  {
    format!("{arch}-unknown-linux-musl").into()
  } else {
    target.into()
  }
}

fn included_build(
  home: &Path,
  directories: &[PathBuf],
  target: &str,
  purpose: Purpose,
) -> io::Result<Bundle> {
  let target = publisher_target(target, purpose);
  let target = target.as_ref();
  let candidate = crate::remote_bundle::read_compatible_bundle(directories, target)
    .map_err(io::Error::other)?
    .ok_or_else(|| {
      invalid(
        "no selected or included compatible build; provide build files or select a bundle in About",
      )
    })?;
  let source = if candidate.bundle_id == candidate.app_version {
    Source::Release
  } else {
    Source::Ci
  };
  crate::components::import_remote(home, &candidate, target, source).map_err(io::Error::other)
}

fn provided_build(home: &Path, path: &Path, target: &str, purpose: Purpose) -> io::Result<Bundle> {
  if path.is_dir() {
    if path.join(MANIFEST_FILE).exists() {
      let bundle = Bundle::open(path)?;
      verify_source(&bundle.manifest, target, purpose)?;
      return Store::new(home).publish(&bundle.manifest, &bundle.read_files()?);
    }
    return included_build(home, &[path.to_owned()], target, purpose);
  }
  let bytes = crate::components::read_input(path, MAX_FILE_BYTES)?;
  let mut files = crate::components::read_archive(&bytes)?;
  if let Some(metadata) = files.remove(MANIFEST_FILE) {
    if metadata.len() > ctl_core::bundles::MAX_MANIFEST_BYTES {
      return Err(invalid("provided manifest exceeds its size limit"));
    }
    let manifest: Manifest = serde_json::from_slice(&metadata).map_err(io::Error::other)?;
    verify_source(&manifest, target, purpose)?;
    return Store::new(home).publish(&manifest, &files);
  }
  // A publisher archive needs its matching bundle-set.json next to it. Verify
  // the named file, rather than accidentally accepting a different sibling.
  let parent = path
    .parent()
    .ok_or_else(|| invalid("provided archive has no parent"))?;
  let target = publisher_target(target, purpose);
  let target = target.as_ref();
  let candidate = crate::remote_bundle::read_compatible_bundle(&[parent.to_owned()], target)
    .map_err(io::Error::other)?
    .ok_or_else(|| invalid("publisher archives need their matching bundle-set.json"))?;
  if candidate.archive != bytes {
    return Err(invalid(
      "provided archive differs from the target named by bundle-set.json",
    ));
  }
  let source = if candidate.bundle_id == candidate.app_version {
    Source::Release
  } else {
    Source::Ci
  };
  crate::components::import_remote(home, &candidate, target, source).map_err(io::Error::other)
}

/// Packages only the agent from a verified complete source; retained daemons
/// are represented by installation links, never copied from this new build.
///
/// # Errors
/// Rejects modified source bytes or incompatible contracts.
pub fn agent_archive(bundle: &Bundle) -> io::Result<Vec<u8>> {
  if !crate::components::compatible(&bundle.manifest.components) {
    return Err(invalid("agent source is incompatible with this client"));
  }
  let files = bundle.read_files()?;
  let source = AgentSource {
    schema_version: 1,
    source_bundle: bundle.manifest.clone(),
  };
  let agent = &files["ctl-agent"];
  source.verify(agent)?;
  let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
  let mut archive = tar::Builder::new(encoder);
  crate::components::append(&mut archive, "ctl-agent", agent, true)?;
  crate::components::append(
    &mut archive,
    SOURCE_FILE,
    &serde_json::to_vec(&source).map_err(io::Error::other)?,
    false,
  )?;
  let bytes = archive.into_inner()?.finish()?;
  if bytes.len() > MAX_FILE_BYTES {
    return Err(invalid("agent archive exceeds its size limit"));
  }
  Ok(bytes)
}

pub(crate) fn inspect_agent_archive(bytes: &[u8]) -> io::Result<AgentSource> {
  let mut files = crate::components::read_archive(bytes)?;
  let metadata = files
    .remove(SOURCE_FILE)
    .ok_or_else(|| invalid("missing agent source"))?;
  if metadata.len() > ctl_core::bundles::MAX_MANIFEST_BYTES || files.len() != 1 {
    return Err(invalid(
      "agent package must contain only its binary and source record",
    ));
  }
  let source: AgentSource = serde_json::from_slice(&metadata).map_err(io::Error::other)?;
  source.verify(
    files
      .get("ctl-agent")
      .ok_or_else(|| invalid("missing agent binary"))?,
  )?;
  Ok(source)
}

/// Installs locally without stopping any owner. Local full bundles still pass
/// the platform's signed-helper validation before changing the local profile.
///
/// # Errors
/// Rejects wrong targets, invalid signing or failed atomic activation.
pub async fn install_local(
  home: &Path,
  bundle: &Bundle,
  package: Package,
) -> io::Result<UpdateResult> {
  if !ctl_core::bundles::local_target(&bundle.manifest.target_triple) {
    return Err(invalid("this build cannot run on the local host"));
  }
  match package {
    Package::FullBundle => {
      // Validate signing and commit the local service profile before activation.
      // If the second pointer cannot switch, report the completed profile change
      // explicitly; never imply that the selected daemons were rolled back.
      crate::components::select(home, Purpose::Local, bundle).await?;
      crate::ssh_install::install_local_bundle(home, bundle).await.map_err(|error| io::Error::other(format!("Local services now select this bundle, but agent activation failed: {error}. Running services were preserved; refresh Components before retrying.")))?;
    }
    Package::CtlAgent => {
      let snapshot = bundle.clone();
      let account = home.to_owned();
      let (archive, companions) = tokio::task::spawn_blocking(move || {
        let archive = agent_archive(&snapshot)?;
        let companions = Store::new(&account)
          .selected(Purpose::Local, ctl_core::paths::native_target())?
          .map(|bundle| bundle.directory);
        Ok::<_, io::Error>((archive, companions))
      })
      .await
      .map_err(io::Error::other)??;
      crate::ssh_install::install_local_agent(home, companions.as_deref(), &archive)
        .await
        .map_err(io::Error::other)?;
    }
  }
  Ok(result(bundle, package))
}

#[must_use]
pub fn result(bundle: &Bundle, package: Package) -> UpdateResult {
  UpdateResult {
    package,
    bundle_id: bundle.manifest.bundle_id.clone(),
    target_triple: bundle.manifest.target_triple.clone(),
    services_preserved: true,
  }
}

/// Agent-only activation can be verified on compatible older agents that do
/// not yet recognize the new on-disk source record.
#[must_use]
pub fn agent_matches(
  expected: &ctl_core::component::ComponentInfo,
  actual: &ctl_proto::RemoteIdentity,
) -> bool {
  actual.is_valid()
    && actual.build.as_ref() == Some(&expected.build)
    && ctl_core::component::protocols_match(&expected.protocols, &actual.protocols)
}

/// Uploads the chosen package from a previously verified complete source.
///
/// # Errors
/// Rejects changed bytes, unsupported targets or failed remote activation.
pub async fn install_remote(
  destination: &str,
  options: &crate::SshConnectionOptions,
  interaction: &crate::SshInteraction,
  bundle: &Bundle,
  package: Package,
  on_progress: impl Fn(crate::RemoteInstallProgress) + Send + Sync,
) -> Result<UpdateResult, crate::CoreError> {
  use crate::{RemoteInstallEvent as Event, RemoteInstallPhase as Phase, RemoteInstallProgress};
  use std::sync::atomic::{AtomicU64, Ordering};
  let invalid = |error: io::Error| crate::CoreError::InvalidComponentBundle(error.to_string());
  let snapshot = bundle.clone();
  let archive = tokio::task::spawn_blocking(move || match package {
    Package::CtlAgent => agent_archive(&snapshot),
    Package::FullBundle => crate::components::upload_bundle(&snapshot).map(|upload| upload.archive),
  })
  .await
  .map_err(|error| invalid(io::Error::other(error)))?
  .map_err(invalid)?;
  let transferred = AtomicU64::new(0);
  on_progress(RemoteInstallProgress {
    phase: Phase::Connecting,
    total_bytes: archive.len() as u64,
    ..RemoteInstallProgress::default()
  });
  let report = |event| {
    let (phase, file_name) = match event {
      Event::Receiving { received_bytes } => {
        transferred.store(received_bytes, Ordering::Relaxed);
        (Phase::Transferring, None)
      }
      Event::Extracting => (Phase::Extracting, None),
      Event::Checking { file_name } => (Phase::Checking, Some(file_name.to_owned())),
      Event::Activating => (Phase::Activating, None),
      Event::Complete => (Phase::Complete, None),
    };
    on_progress(RemoteInstallProgress {
      phase,
      file_name,
      transferred_bytes: transferred.load(Ordering::Relaxed),
      total_bytes: archive.len() as u64,
      bytes_per_second: 0,
    });
  };
  match package {
    Package::CtlAgent => {
      crate::install_ssh_agent_only_with_progress(
        destination,
        options,
        interaction,
        &archive,
        report,
      )
      .await?;
    }
    Package::FullBundle => {
      let id = bundle
        .manifest
        .distribution_id
        .as_ref()
        .unwrap_or(&bundle.manifest.bundle_id);
      crate::install_ssh_unix_agent_interactive_with_progress(
        destination,
        options,
        interaction,
        id,
        &archive,
        report,
      )
      .await?;
    }
  }
  Ok(result(bundle, package))
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
