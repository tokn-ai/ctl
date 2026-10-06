//! Standalone discovery separates helper compatibility from build provenance.

use super::{Error, install::Session, macos};
use ctl_core::component::ComponentInfo;
use ctl_core::protocol::ProtocolVersion;
use std::path::{Path, PathBuf};

pub(super) async fn discover(home: &Path) -> Result<Option<PathBuf>, Error> {
  discover_required(home, None).await
}

pub(super) async fn discover_for_helper_contract(
  home: &Path,
  required: ProtocolVersion,
) -> Result<Option<PathBuf>, Error> {
  discover_required(home, Some(required)).await
}

async fn discover_required(
  home: &Path,
  required: Option<ProtocolVersion>,
) -> Result<Option<PathBuf>, Error> {
  let target = macos::release_target()?;
  if let Some(bundle) =
    ctl_core::bundles::Store::new(home).selected(ctl_core::bundles::Purpose::Local, target)?
  {
    let files = bundle.read_files()?;
    let receipt = files.get("ctld-package.json").ok_or_else(|| {
      Error::Verification("selected bundle lacks its signed helper receipt".into())
    })?;
    let info = super::inspect_ctld_package(home, &bundle.directory, receipt).await?;
    if !compatible(&info) || !bundle.manifest.same_component("ctld", &info) {
      return Err(Error::Verification(
        "selected bundle helper is incompatible or differs from its manifest".into(),
      ));
    }
    if let Some(required) = required
      && !compatible_for_helper_contract(&info, Some(required))
    {
      return Err(Error::Verification(format!(
        "selected bundle helper does not advertise required ctld_helper contract {required}; provision and select a bundle that supports it"
      )));
    }
    return Ok(Some(bundle.directory.join("ctld.app/Contents/MacOS/ctld")));
  }
  let Some(installation) = ctl_ipc::managed::resolve_compatible_installation(home, target)? else {
    return Ok(None);
  };
  let session = Session::open_installed(home, target, &installation)?;
  let prepared = macos::verify_helper(&session).await?;
  if !compatible_for_helper_contract(&prepared.info, required) {
    return Ok(None);
  }
  // The path is pinned to an immutable cache entry, independent of a concurrent
  // change to the shared selection. Discovery never activates or restarts it.
  Ok(Some(prepared.path))
}

pub(super) fn compatible_for_helper_contract(
  info: &ComponentInfo,
  required: Option<ProtocolVersion>,
) -> bool {
  compatible(info)
    && required.is_none_or(|required| {
      info
        .protocols
        .iter()
        .any(|entry| entry.name == "ctld_helper" && entry.supports(required))
    })
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

  fn with_helper_contracts(
    latest: ProtocolVersion,
    supported: &[ProtocolVersion],
  ) -> ComponentInfo {
    let mut info = info();
    let helper = info
      .protocols
      .iter_mut()
      .find(|entry| entry.name == "ctld_helper")
      .unwrap();
    *helper = ProtocolInfo::new("ctld_helper", latest.build, latest, supported);
    assert!(info.is_valid());
    info
  }

  #[test]
  fn helper_operations_require_the_exact_advertised_contract() {
    let old = ctl_ipc::HELPER_API_CONTRACT_V1_0_1;
    let required = ctl_ipc::HELPER_API_CONTRACT_V1_1_3;
    let newer = ProtocolVersion::new(1, 1, ctl_ipc::HELPER_API_BUILD + 1);
    let cases: &[(ProtocolVersion, &[ProtocolVersion], bool, bool)] = &[
      (old, &[old], true, false),
      (required, &[old, required], true, true),
      (newer, &[old, required, newer], true, true),
      (newer, &[old, newer], true, false),
      (newer, &[newer], false, false),
    ];
    for (latest, supported, generally_compatible, operation_compatible) in cases {
      let info = with_helper_contracts(*latest, supported);
      assert_eq!(
        compatible_for_helper_contract(&info, None),
        *generally_compatible,
        "latest {latest}, contracts {supported:?}"
      );
      assert_eq!(
        compatible_for_helper_contract(&info, Some(required)),
        *operation_compatible,
        "latest {latest}, contracts {supported:?}"
      );
      assert_eq!(
        compatible_for_helper_contract(&info, Some(old)),
        *generally_compatible,
        "latest {latest}, contracts {supported:?}"
      );
    }
  }

  #[test]
  fn operation_support_does_not_override_other_compatibility_requirements() {
    let required = ctl_ipc::HELPER_API_CONTRACT_V1_1_3;
    let mut missing = info();
    assert!(compatible_for_helper_contract(&missing, Some(required)));
    missing
      .protocols
      .retain(|entry| entry.name != "ctld_lifecycle");
    assert!(!compatible_for_helper_contract(&missing, Some(required)));
    let mut duplicate = info();
    let helper = duplicate
      .protocols
      .iter()
      .find(|entry| entry.name == "ctld_helper")
      .unwrap()
      .clone();
    duplicate.protocols.push(helper);
    assert!(!compatible_for_helper_contract(&duplicate, Some(required)));
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
    assert!(
      discover_for_helper_contract(&home.0, ctl_ipc::HELPER_API_VERSION)
        .await
        .unwrap()
        .is_none()
    );
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
    assert!(matches!(
      discover_for_helper_contract(&home.0, ctl_ipc::HELPER_API_VERSION).await,
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
