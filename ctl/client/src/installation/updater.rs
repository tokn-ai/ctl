use ctl_core::bundles::Bundle;
use std::io;
use std::path::{Path, PathBuf};

/// Resolve a verified target updater, independently of the active runtime build.
/// Native executions can bootstrap from their installed/sibling manager. Foreign
/// targets need an explicit sidecar or the updater from a trusted publication.
///
/// # Errors
/// Rejects unsafe/corrupt candidates or missing target publications.
pub async fn updater_for_bundle(home: &Path, bundle: &Bundle) -> io::Result<Vec<u8>> {
  updater_for_manifest(home, &bundle.manifest).await
}

/// Provision the independent native manager for both CLI and desktop installs.
/// Runtime activation never changes this entry point during a rollback.
///
/// # Errors
/// Rejects foreign installs, unavailable managers, unsafe storage or an invalid
/// executable. A failed probe leaves the existing manager untouched.
pub async fn provision_manager(home: &Path, bundle: &Bundle) -> io::Result<()> {
  if !ctl_core::bundles::local_target(&bundle.manifest.target_triple) {
    return Err(io::Error::other(
      "manager provisioning requires a native installation",
    ));
  }
  let executable = std::env::current_exe()?;
  let bytes = if executable
    .file_stem()
    .and_then(|name| name.to_str())
    .is_some_and(|name| name == "ctlup" || name.starts_with("ctlup-"))
  {
    native_updater(&executable).await?
  } else {
    updater_for_bundle(home, bundle).await?
  };
  let directory = home.join(".tokn/ctl/bin");
  let stage = tokio::task::spawn_blocking(move || {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    super::safe_directory(&directory)?;
    manager_destination(&directory.join("ctlup"))?;
    let stage = TemporaryManager(directory.join(format!(".ctlup-{}", uuid::Uuid::new_v4())));
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o700)
      .open(&stage.0)?;
    file.write_all(&bytes)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    file.sync_all()?;
    Ok::<_, io::Error>(stage)
  })
  .await
  .map_err(io::Error::other)??;
  // Probe the exact staged bytes, including architecture and command identity.
  native_updater(&stage.0).await?;
  tokio::task::spawn_blocking(move || {
    let directory = stage
      .0
      .parent()
      .ok_or_else(|| io::Error::other("missing manager directory"))?;
    manager_destination(&directory.join("ctlup"))?;
    std::fs::rename(&stage.0, directory.join("ctlup"))?;
    std::fs::File::open(directory)?.sync_all()
  })
  .await
  .map_err(io::Error::other)?
}

fn manager_destination(path: &Path) -> io::Result<()> {
  use std::os::unix::fs::MetadataExt as _;
  match std::fs::symlink_metadata(path) {
    Ok(metadata)
      if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o022 != 0 =>
    {
      Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "manager entry point must be an owned regular file, not a link or writable by others",
      ))
    }
    Ok(_) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

struct TemporaryManager(PathBuf);
impl Drop for TemporaryManager {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

/// Resolve a target updater for a verified managed archive before importing it.
///
/// # Errors
/// Reports invalid metadata, corrupt candidates or unavailable publications.
pub async fn updater_for_manifest(
  home: &Path,
  manifest: &ctl_core::bundles::Manifest,
) -> io::Result<Vec<u8>> {
  manifest.validate()?;
  let target = &manifest.target_triple;
  if let Some(bytes) = supplied_updater(home, manifest)? {
    return Ok(bytes);
  }
  if ctl_core::bundles::local_target(target)
    && let Some(path) = std::env::var_os("CTLUP_BIN")
  {
    return native_updater(Path::new(&path)).await;
  }
  if target == ctl_core::paths::native_target() {
    let executable = std::env::current_exe()?;
    let source = if executable.file_stem().is_some_and(|name| name == "ctlup") {
      executable.clone()
    } else {
      executable.with_file_name("ctlup")
    };
    for path in [source, home.join(".tokn/ctl/bin/ctlup")] {
      match native_updater(&path).await {
        Ok(bytes) => return Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
      }
    }
  }
  let mut directories = Vec::new();
  if let Some(directory) = std::env::var_os("CTL_REMOTE_BUNDLES_DIR") {
    directories.push(PathBuf::from(directory));
  }
  if let Some(directory) = std::env::var_os("CTLUP_BUNDLES_DIR") {
    directories.push(PathBuf::from(directory));
  }
  if let Some(workspace) = crate::component_update::development_workspace() {
    directories.push(
      workspace
        .join("target/development-bundles/updaters")
        .join(target),
    );
    directories.push(workspace.join("apps/desktop/src-tauri/resources/agent-bundles"));
  }
  if let Some(parent) = std::env::current_exe()?.parent() {
    directories.push(parent.join("resources/agent-bundles"));
    directories.push(parent.join("../Resources/agent-bundles"));
  }
  for directory in directories {
    match crate::remote_bundle::published::read_updater(&directory, target) {
      Ok(updater) => return Ok(updater.bytes),
      Err(crate::remote_bundle::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(io::Error::other(error)),
    }
  }
  let channel = match manifest.source {
    ctl_core::bundles::Source::Release
      if !manifest.components["ctl-agent"]
        .build
        .version
        .split('+')
        .next()
        .unwrap_or_default()
        .contains('-') =>
    {
      ctl_core::component_update::BundleChannel::Stable
    }
    ctl_core::bundles::Source::Ci | ctl_core::bundles::Source::Release => {
      ctl_core::component_update::BundleChannel::Dev
    }
    ctl_core::bundles::Source::Local => super::default_channel(home)?,
  };
  let updater = crate::remote_bundle::published::download_updater_cached(
    channel,
    target,
    &home.join(".tokn/ctl/cache/updaters"),
  )
  .await
  .map_err(io::Error::other)?;
  Ok(updater.bytes)
}

pub(crate) fn remember_supplied_updater(
  home: &Path,
  bundle: &Bundle,
  directory: &Path,
) -> io::Result<()> {
  let updater = match crate::remote_bundle::published::read_updater(
    directory,
    &bundle.manifest.target_triple,
  ) {
    Ok(updater) => updater,
    Err(crate::remote_bundle::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
      return Ok(());
    }
    Err(error) => return Err(io::Error::other(error)),
  };
  let root = home.join(".tokn/ctl/cache/updaters");
  crate::remote_bundle::published::cache_verified(&root, &updater).map_err(io::Error::other)?;
  let records = root.join("records");
  super::safe_directory(&records)?;
  let stage = records.join(format!(".record-{}", uuid::Uuid::new_v4()));
  let result = (|| {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(&stage)?;
    serde_json::to_writer(&mut file, &updater.info)?;
    file.sync_all()?;
    std::fs::rename(
      &stage,
      records.join(format!("{}.json", bundle.manifest.bundle_id)),
    )?;
    std::fs::File::open(&records)?.sync_all()
  })();
  if result.is_err() {
    let _ = std::fs::remove_file(stage);
  }
  result
}

fn supplied_updater(
  home: &Path,
  manifest: &ctl_core::bundles::Manifest,
) -> io::Result<Option<Vec<u8>>> {
  let root = home.join(".tokn/ctl/cache/updaters");
  if !super::storage_exists(&root.join("records"))? {
    return Ok(None);
  }
  let metadata = match crate::components::read_input(
    &root
      .join("records")
      .join(format!("{}.json", manifest.bundle_id)),
    64 * 1024,
  ) {
    Ok(bytes) => bytes,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  let info =
    crate::remote_bundle::published::UpdaterInfo::parse(&metadata, &manifest.target_triple)
      .map_err(io::Error::other)?;
  crate::remote_bundle::published::read_cached_updater(&root, &info)
    .map(|updater| updater.map(|updater| updater.bytes))
    .map_err(io::Error::other)
}

async fn native_updater(path: &Path) -> io::Result<Vec<u8>> {
  let before = crate::components::read_input(path, ctl_core::bundles::MAX_FILE_BYTES)?;
  let info = ctl_core::executable::inspect(path).await?;
  if !info.is_valid() {
    return Err(io::Error::other("invalid ctlup component metadata"));
  }
  verify_updater_name(path).await?;
  if before != crate::components::read_input(path, ctl_core::bundles::MAX_FILE_BYTES)? {
    return Err(io::Error::other("ctlup changed while preparing deployment"));
  }
  Ok(before)
}

async fn verify_updater_name(path: &Path) -> io::Result<()> {
  use std::process::Stdio;
  let mut child = tokio::process::Command::new(path)
    .arg("--version")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| io::Error::other("missing updater version output"))?;
  let stderr = child
    .stderr
    .take()
    .ok_or_else(|| io::Error::other("missing updater version diagnostics"))?;
  let (status, stdout, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
    tokio::try_join!(child.wait(), read_version(stdout), read_version(stderr))
  })
  .await
  .map_err(|_| {
    io::Error::new(
      io::ErrorKind::TimedOut,
      "updater version inspection timed out",
    )
  })??;
  if !status.success() || !stdout.starts_with(b"ctlup ") {
    return Err(io::Error::other("selected updater executable is not ctlup"));
  }
  Ok(())
}

async fn read_version(reader: impl tokio::io::AsyncRead + Unpin) -> io::Result<Vec<u8>> {
  use tokio::io::AsyncReadExt as _;
  let mut bytes = Vec::new();
  reader.take(4097).read_to_end(&mut bytes).await?;
  if bytes.len() > 4096 {
    return Err(io::Error::other(
      "updater version output exceeds its size limit",
    ));
  }
  Ok(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;
  use sha2::{Digest as _, Sha256};
  use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

  #[tokio::test]
  async fn a_runtime_cli_with_valid_metadata_is_not_accepted_as_the_updater() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let root = std::env::temp_dir().join(format!("ctlup-identity-test-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&root)
      .unwrap();
    let path = root.join("candidate");
    let fixture =
      crate::remote_bundle::compatibility::tests::Fixture::new("0.1.0", &"a".repeat(40))
        .with_clients();
    let info = &fixture.outer["targets"]["aarch64-apple-darwin"]["components"]["ctl"];
    for name in ["ctl", "ctlup"] {
      let script = format!(
        "#!/bin/sh\ncase \"$1\" in --component-info) cat <<'METADATA'\n{info}\nMETADATA\n;; --version) printf '{name} 0.1.0\\n';; *) exit 1;; esac\n"
      );
      std::fs::write(&path, &script).unwrap();
      std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
      let result = native_updater(&path).await;
      if name == "ctl" {
        assert!(result.unwrap_err().to_string().contains("not ctlup"));
      } else {
        assert_eq!(result.unwrap(), script.as_bytes());
      }
    }
    std::fs::remove_dir_all(root).unwrap();
  }

  #[tokio::test]
  async fn shared_local_provisioning_checks_the_staged_manager_before_replacing_it() {
    let root = std::env::temp_dir().join(format!("ctlup-provision-test-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&root)
      .unwrap();
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
      ("macos", "aarch64") => "aarch64-apple-darwin",
      ("macos", _) => "x86_64-apple-darwin",
      (_, "aarch64") => "aarch64-unknown-linux-musl",
      _ => "x86_64-unknown-linux-musl",
    };
    let fixture =
      crate::remote_bundle::compatibility::tests::Fixture::new("0.1.0", &"a".repeat(40))
        .with_clients();
    let components =
      serde_json::from_value(fixture.outer["targets"][target]["components"].clone()).unwrap();
    let manifest = ctl_core::bundles::Manifest::new(
      target,
      ctl_core::bundles::Source::Ci,
      components,
      &fixture.files,
    )
    .unwrap();
    let bundle = ctl_core::bundles::Store::cache(&root)
      .publish(&manifest, &fixture.files)
      .unwrap();
    let provided = root.join("provided");
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&provided)
      .unwrap();
    let metadata = &fixture.outer["targets"][target]["components"]["ctl"];
    let binary = format!("ctlup-0.1.0-dev.{}-{target}", "a".repeat(12));
    let mut previous = Vec::new();
    for name in ["ctlup", "ctl"] {
      let script = format!(
        "#!/bin/sh\ncase \"$1\" in --component-info) cat <<'METADATA'\n{metadata}\nMETADATA\n;; --version) printf '{name} 0.1.0\\n';; *) exit 1;; esac\n"
      );
      std::fs::write(provided.join(&binary), &script).unwrap();
      let info = serde_json::json!({
        "schema_version": 1, "app_version": "0.1.0", "bundle_id": format!("0.1.0-dev.{}", "a".repeat(12)),
        "git_revision": "a".repeat(40), "target_triple": target, "binary": binary,
        "sha256": format!("{:x}", Sha256::digest(script.as_bytes())), "size": script.len(),
      });
      std::fs::write(
        provided.join(format!("ctlup-{target}.json")),
        serde_json::to_vec(&info).unwrap(),
      )
      .unwrap();
      remember_supplied_updater(&root, &bundle, &provided).unwrap();
      let outcome = provision_manager(&root, &bundle).await;
      let installed = root.join(".tokn/ctl/bin/ctlup");
      if name == "ctlup" {
        outcome.unwrap();
        previous = std::fs::read(&installed).unwrap();
        assert_eq!(previous, script.as_bytes());
        assert!(
          !std::fs::symlink_metadata(&installed)
            .unwrap()
            .file_type()
            .is_symlink()
        );
      } else {
        assert!(outcome.unwrap_err().to_string().contains("not ctlup"));
        assert_eq!(std::fs::read(installed).unwrap(), previous);
      }
    }
    std::fs::remove_dir_all(root).unwrap();
  }
}
