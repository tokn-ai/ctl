//! Standalone discovery separates helper compatibility from build provenance.

use super::{Error, install::Session, macos};
use ctl_core::component::ComponentInfo;
use std::path::{Path, PathBuf};

pub(super) async fn discover(home: &Path) -> Result<Option<PathBuf>, Error> {
  let target = macos::release_target()?;
  let Some(installation) = ctl_ipc::managed::resolve_compatible_installation(home, target)? else {
    return Ok(None);
  };
  let session = Session::open_installed(home, target, &installation)?;
  let prepared = macos::verify_helper(&session).await?;
  if !compatible(&prepared.info) {
    return Ok(None);
  }
  // The path is pinned to an immutable cache entry, independent of a concurrent
  // change to the shared selection. Discovery never activates or restarts it.
  Ok(Some(prepared.path))
}

pub(super) fn compatible(info: &ComponentInfo) -> bool {
  info.is_valid()
    && [
      ("ctld", ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS),
      (
        "ctld_lifecycle",
        ctl_ipc::lifecycle::SUPPORTED_PROTOCOL_VERSIONS,
      ),
      ("ctld_helper", ctl_ipc::SUPPORTED_HELPER_API_VERSIONS),
    ]
    .iter()
    .all(|(name, required)| {
      let mut entries = info.protocols.iter().filter(|entry| entry.name == *name);
      entries
        .next()
        .is_some_and(|entry| entry.negotiate(required).is_some())
        && entries.next().is_none()
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctl_core::component::ProtocolInfo;

  fn info() -> ComponentInfo {
    let mut build = ctl_core::component::build_info();
    build.version = "0.9.0".into();
    build.source_fingerprint = "a".repeat(64);
    ComponentInfo {
      build,
      protocols: vec![
        ProtocolInfo::new(
          "ctld",
          ctl_ipc::PROTOCOL_BUILD,
          ctl_ipc::PROTOCOL_VERSION,
          ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS,
        ),
        ProtocolInfo::new(
          "ctld_lifecycle",
          ctl_ipc::lifecycle::PROTOCOL_BUILD,
          ctl_ipc::lifecycle::PROTOCOL_VERSION,
          ctl_ipc::lifecycle::SUPPORTED_PROTOCOL_VERSIONS,
        ),
        ProtocolInfo::new(
          "ctld_helper",
          ctl_ipc::HELPER_API_BUILD,
          ctl_ipc::HELPER_API_VERSION,
          ctl_ipc::SUPPORTED_HELPER_API_VERSIONS,
        ),
      ],
    }
  }

  #[test]
  fn compatible_apis_allow_a_different_client_release_and_source_build() {
    let info = info();
    assert!(compatible(&info));
    assert_ne!(info.build.version, env!("CARGO_PKG_VERSION"));
    for index in 0..info.protocols.len() {
      let mut missing = info.clone();
      missing.protocols.remove(index);
      assert!(!compatible(&missing));
      let mut different = info.clone();
      let protocol = &mut different.protocols[index];
      let version = ctl_core::protocol::ProtocolVersion::new(2, 0, protocol.build);
      protocol.version = version;
      protocol.supported_versions = vec![version];
      assert!(!compatible(&different));
      let mut duplicate = info.clone();
      duplicate.protocols.push(duplicate.protocols[index].clone());
      assert!(!compatible(&duplicate));
    }
  }

  #[test]
  fn newer_helpers_are_reused_only_for_explicitly_advertised_common_contracts() {
    let mut info = info();
    for protocol in &mut info.protocols {
      let common = protocol.version;
      let newer = ctl_core::protocol::ProtocolVersion::new(1, 1, protocol.build + 1);
      protocol.build += 1;
      protocol.version = newer;
      protocol.supported_versions = vec![common, newer];
    }
    assert!(compatible(&info));
    for index in 0..info.protocols.len() {
      let mut unsupported = info.clone();
      unsupported.protocols[index].supported_versions.remove(0);
      assert!(unsupported.is_valid());
      assert!(!compatible(&unsupported));
    }
  }

  #[tokio::test]
  async fn missing_selection_does_not_create_an_installation() {
    let home = super::super::tests::Home::new();
    assert!(discover(&home.0).await.unwrap().is_none());
    assert!(!home.0.join(".tokn").exists());
  }

  #[tokio::test]
  async fn selected_unsigned_helper_is_rejected_before_metadata_execution() {
    use super::super::tests::{Home, compressed, contents, release};
    let home = Home::new();
    let bytes = compressed(&contents(None));
    let mut manifest = release(&bytes);
    manifest.target = macos::release_target().unwrap().into();
    manifest.archive = format!(
      "ctld-{}-{}.app.tar.gz",
      manifest.app_version, manifest.target
    );
    let installed = Session::begin(&home.0, manifest)
      .unwrap()
      .unpack(&bytes)
      .unwrap()
      .activate()
      .unwrap();
    let executed = home.0.join("metadata-executed");
    std::fs::write(
      &installed.executable,
      format!("#!/bin/sh\n/usr/bin/touch '{}'\n", executed.display()),
    )
    .unwrap();
    assert!(matches!(
      discover(&home.0).await,
      Err(Error::Verification(_))
    ));
    assert!(!executed.exists());
    let directory = ctl_ipc::managed::component_directory(&home.0);
    assert!(!std::fs::read_dir(directory).unwrap().any(|entry| {
      entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".setup-")
    }));
  }
}
