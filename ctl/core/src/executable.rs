//! Bounded inspection and identity checks for a selected replacement executable.

use crate::component::ComponentInfo;
use crate::protocol::ProtocolVersion;
use sha2::{Digest as _, Sha256};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt as _;

#[derive(Debug, Clone)]
pub struct PreparedExecutable {
  pub path: PathBuf,
  pub info: ComponentInfo,
  selected: PathBuf,
  digest: Vec<u8>,
}

/// Reads a selected installed helper's bounded metadata without starting its service.
///
/// # Errors
/// Rejects missing helpers, invalid metadata, or a changing selection.
pub async fn inspect(selected: &Path) -> io::Result<ComponentInfo> {
  let path = resolve(selected)?;
  let info = metadata(&path).await?;
  if !info.is_valid() || resolve(selected)? != path {
    return Err(io::Error::other(
      "Invalid or changing installed component metadata",
    ));
  }
  Ok(info)
}

impl PreparedExecutable {
  /// Resolves and verifies a replacement without starting its daemon service.
  ///
  /// # Errors
  /// Rejects unavailable, changing, malformed or incompatible executables.
  pub async fn prepare(
    selected: PathBuf,
    required: &[(&str, &[ProtocolVersion])],
  ) -> io::Result<Self> {
    let path = resolve(&selected)?;
    let digest = fingerprint(path.clone()).await?;
    let info = metadata(&path).await?;
    if !info.is_valid()
      || required.iter().any(|(name, versions)| {
        let entries: Vec<_> = info
          .protocols
          .iter()
          .filter(|entry| entry.name == *name)
          .collect();
        entries.len() != 1 || entries[0].negotiate(versions).is_none()
      })
    {
      return Err(io::Error::other(
        "The selected helper reports incompatible component metadata",
      ));
    }
    if fingerprint(path.clone()).await? != digest {
      return Err(io::Error::other(
        "The selected helper changed during inspection",
      ));
    }
    Ok(Self {
      path,
      info,
      selected,
      digest,
    })
  }

  /// Checks the selection and file contents again immediately before mutation.
  ///
  /// # Errors
  /// Returns an error if the selected executable was removed or changed.
  pub async fn verify(&self) -> io::Result<()> {
    if resolve(&self.selected)? != self.path || fingerprint(self.path.clone()).await? != self.digest
    {
      return Err(io::Error::other(
        "The selected helper changed; prepare the restart again",
      ));
    }
    Ok(())
  }

  /// Compares observed successor metadata, independent of advertisement order.
  #[must_use]
  pub fn matches(&self, observed: &ComponentInfo) -> bool {
    observed.is_valid()
      && self.info.build == observed.build
      && crate::component::protocols_match(&self.info.protocols, &observed.protocols)
  }
}

fn resolve(selected: &Path) -> io::Result<PathBuf> {
  if selected.components().count() > 1 || selected.is_absolute() {
    return selected.canonicalize();
  }
  for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
    let candidate = directory.join(selected);
    if candidate.is_file() {
      return candidate.canonicalize();
    }
    #[cfg(windows)]
    if candidate.extension().is_none() {
      let executable = candidate.with_extension("exe");
      if executable.is_file() {
        return executable.canonicalize();
      }
    }
  }
  Err(io::Error::new(
    io::ErrorKind::NotFound,
    "The selected helper was not found on PATH",
  ))
}

async fn fingerprint(path: PathBuf) -> io::Result<Vec<u8>> {
  tokio::task::spawn_blocking(move || {
    let mut file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
      return Err(io::Error::other(
        "The selected helper is not a regular executable file",
      ));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
      let count = file.read(&mut buffer)?;
      if count == 0 {
        break;
      }
      digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().to_vec())
  })
  .await
  .map_err(io::Error::other)?
}

async fn metadata(path: &Path) -> io::Result<ComponentInfo> {
  tokio::time::timeout(Duration::from_secs(3), async {
    let mut child = tokio::process::Command::new(path)
      .arg("--component-info")
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()?;
    let mut stdout = child
      .stdout
      .take()
      .ok_or_else(|| io::Error::other("Missing helper output"))?
      .take(16 * 1024 + 1);
    let mut output = Vec::new();
    stdout.read_to_end(&mut output).await?;
    if output.len() > 16 * 1024 {
      return Err(io::Error::other(
        "The selected helper returned oversized metadata",
      ));
    }
    if !child.wait().await?.success() {
      return Err(io::Error::other(
        "The selected helper does not support component metadata; update or rebuild it first",
      ));
    }
    serde_json::from_slice(&output).map_err(io::Error::other)
  })
  .await
  .map_err(|_| {
    io::Error::new(
      io::ErrorKind::TimedOut,
      "The selected helper did not answer the version query",
    )
  })?
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use crate::component::ProtocolInfo;
  use std::os::unix::fs::PermissionsExt as _;
  use std::sync::atomic::{AtomicUsize, Ordering};

  const OLD: ProtocolVersion = ProtocolVersion::new(1, 0, 13);
  const NEW: ProtocolVersion = ProtocolVersion::new(1, 1, 15);

  static NEXT: AtomicUsize = AtomicUsize::new(0);

  #[tokio::test]
  async fn metadata_query_ignores_inherited_askpass_modes() {
    let _guard = crate::test_fixtures::ProcessGuard::acquire().await;
    let directory = std::env::temp_dir().join(format!(
      "component-metadata-environment-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("helper");
    let info = ComponentInfo {
      build: crate::component::build_info(),
      protocols: vec![ProtocolInfo::new("test", 13, OLD, &[OLD])],
    };
    // Actual daemon askpass dispatch happens before command-line parsing. A
    // query must clear both inherited selectors before launching the helper.
    std::fs::write(
      &path,
      format!(
        "#!/bin/sh\nset -eu\n[ \"$1\" = --component-info ]\n[ \"${{CTLD_ASKPASS:-}}\" != 1 ]\n[ \"${{CTLD_IDENTITY_ASKPASS:-}}\" != 1 ]\nprintf '%s\\n' '{}'\n",
        serde_json::to_string(&info).unwrap()
      ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
      .args([
        "--exact",
        "executable::tests::metadata_query_child",
        "--nocapture",
      ])
      .env("CTL_METADATA_TEST_EXECUTABLE", &path)
      .env("CTLD_ASKPASS", "1")
      .env("CTLD_IDENTITY_ASKPASS", "1")
      .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), child.output())
      .await
      .unwrap()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    std::fs::remove_dir_all(directory).unwrap();
  }

  #[tokio::test]
  async fn metadata_query_child() {
    let _guard = crate::test_fixtures::ProcessGuard::acquire().await;
    let Some(executable) = std::env::var_os("CTL_METADATA_TEST_EXECUTABLE") else {
      return;
    };
    assert_eq!(std::env::var("CTLD_ASKPASS").unwrap(), "1");
    assert_eq!(std::env::var("CTLD_IDENTITY_ASKPASS").unwrap(), "1");
    PreparedExecutable::prepare(PathBuf::from(executable), &[("test", &[OLD])])
      .await
      .unwrap();
  }

  #[tokio::test]
  async fn recheck_detects_changed_contents_even_with_identical_reported_build() {
    let _guard = crate::test_fixtures::ProcessGuard::acquire().await;
    let root = std::env::temp_dir().join(format!(
      "component-executable-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("helper");
    let info = ComponentInfo {
      build: crate::component::build_info(),
      protocols: vec![ProtocolInfo::new("test", 13, OLD, &[OLD])],
    };
    let contents = format!(
      "#!/bin/sh\nprintf '%s\\n' '{}'\n",
      serde_json::to_string(&info).unwrap()
    );
    std::fs::write(&path, &contents).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let prepared = PreparedExecutable::prepare(path.clone(), &[("test", &[OLD])])
      .await
      .unwrap();
    prepared.verify().await.unwrap();
    std::fs::write(&path, format!("{contents}# changed executable\n")).unwrap();
    assert!(prepared.verify().await.is_err());
    assert!(
      PreparedExecutable::prepare(path, &[("test", &[NEW])])
        .await
        .is_err()
    );
    std::fs::remove_dir_all(root).unwrap();
  }

  #[tokio::test]
  async fn helper_inspection_negotiates_explicit_supported_contracts() {
    let _guard = crate::test_fixtures::ProcessGuard::acquire().await;
    let root = std::env::temp_dir().join(format!(
      "component-contracts-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("helper");
    for (build, version, supported, required) in [
      (13, OLD, vec![OLD], vec![OLD, NEW]),
      (15, NEW, vec![OLD, NEW], vec![OLD]),
    ] {
      let info = ComponentInfo {
        build: crate::component::build_info(),
        protocols: vec![ProtocolInfo::new("test", build, version, &supported)],
      };
      std::fs::write(
        &path,
        format!(
          "#!/bin/sh\nprintf '%s\\n' '{}'\n",
          serde_json::to_string(&info).unwrap()
        ),
      )
      .unwrap();
      std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
      let prepared = PreparedExecutable::prepare(path.clone(), &[("test", &required)])
        .await
        .unwrap();
      assert_eq!(prepared.info, info);
      let mut reordered = info.clone();
      reordered.protocols[0].supported_versions.reverse();
      assert!(prepared.matches(&reordered));
      reordered.protocols[0].supported_versions = vec![version];
      assert_eq!(prepared.matches(&reordered), supported.len() == 1);
      assert!(
        PreparedExecutable::prepare(path.clone(), &[("test", &[ProtocolVersion::new(2, 0, 17)])])
          .await
          .is_err()
      );
    }
    std::fs::remove_dir_all(root).unwrap();
  }
}
