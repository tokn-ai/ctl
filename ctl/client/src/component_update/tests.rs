use super::*;
use crate::remote_bundle::compatibility::tests::Fixture;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-update-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&path)
      .unwrap();
    Self(path)
  }
}
impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn bundle(home: &Path, revision: char) -> Bundle {
  let fixture = Fixture::new("0.1.0", &revision.to_string().repeat(40)).with_clients();
  let components =
    serde_json::from_value(fixture.outer["targets"]["aarch64-apple-darwin"]["components"].clone())
      .unwrap();
  let manifest = Manifest::new(
    ctl_core::paths::native_target(),
    Source::Ci,
    components,
    &fixture.files,
  )
  .unwrap();
  Store::new(home).publish(&manifest, &fixture.files).unwrap()
}

#[tokio::test]
async fn remote_repairs_reject_legacy_sources_before_upload_or_selection_changes() {
  let source_home = Home::new();
  let account = Home::new();
  let target = "aarch64-apple-darwin";
  let candidate = Fixture::new("0.0.9", &"c".repeat(40))
    .with_clients()
    .without_shared_services()
    .bundle();
  let legacy =
    crate::components::import_remote(&source_home.0, &candidate, target, Source::Ci).unwrap();
  // Inventory, import, and explicit legacy packaging retain their existing
  // compatibility rules. Current remote services need the additional feature.
  assert!(crate::components::compatible(&legacy.manifest.components));
  assert!(agent_archive(&legacy).is_ok());
  Store::new(&source_home.0)
    .select(Purpose::Upload, &legacy)
    .unwrap();
  let selected = prepare(
    &source_home.0,
    target,
    Purpose::Upload,
    &BuildSource::Selected,
    &[],
  )
  .await
  .unwrap_err();
  assert!(selected.to_string().contains("ctl_transport 1.0.1"));
  let provided = prepare(
    &account.0,
    target,
    Purpose::Upload,
    &BuildSource::Provided {
      path: legacy.directory.clone(),
      local_build: false,
      ctld_package: None,
    },
    &[],
  )
  .await
  .unwrap_err();
  assert!(provided.to_string().contains("ctl_transport 1.0.1"));
  assert!(!account.0.join(".tokn/ctl/components").exists());
  assert_eq!(
    Store::new(&source_home.0)
      .selected(Purpose::Upload, target)
      .unwrap()
      .unwrap()
      .manifest
      .bundle_id,
    legacy.manifest.bundle_id
  );
  for package in [Package::CtlAgent, Package::FullBundle] {
    let error = install_remote(
      "invalid\nhost",
      &crate::SshConnectionOptions::default(),
      &crate::SshInteraction::Batch,
      &legacy,
      package,
      |_| panic!("an unsupported source must fail before starting an upload"),
    )
    .await
    .unwrap_err();
    assert!(
      matches!(error, crate::CoreError::InvalidComponentBundle(message) if message.contains("ctl_transport 1.0.1"))
    );
  }
}

#[test]
fn shared_service_uploads_retain_older_remote_broker_contracts() {
  let fixture = Fixture::new("0.0.9", &"d".repeat(40)).with_clients();
  let target = "aarch64-apple-darwin";
  let mut components: std::collections::BTreeMap<String, ctl_core::component::ComponentInfo> =
    serde_json::from_value(fixture.outer["targets"][target]["components"].clone()).unwrap();
  let ctld = components
    .get_mut("ctld")
    .unwrap()
    .protocols
    .iter_mut()
    .find(|protocol| protocol.name == "ctld")
    .unwrap();
  let retained = ctl_ipc::CONTRACT_V1_0_12;
  *ctld = ctl_core::component::ProtocolInfo::new("ctld", retained.build, retained, &[retained]);
  let manifest = Manifest::new(target, Source::Ci, components, &fixture.files).unwrap();
  verify_source(&manifest, target, Purpose::Upload).unwrap();
}

#[tokio::test]
async fn agent_only_retains_the_exact_services_and_clients_across_repeated_updates() {
  let home = Home::new();
  let original = bundle(&home.0, 'a');
  let replacement = bundle(&home.0, 'b');
  let archive = agent_archive(&replacement).unwrap();
  let payload = crate::components::read_archive(&archive).unwrap();
  assert_eq!(
    payload.keys().map(String::as_str).collect::<Vec<_>>(),
    [SOURCE_FILE, "ctl-agent"]
  );
  crate::ssh_install::install_local_agent(&home.0, Some(&original.directory), &archive)
    .await
    .unwrap();
  let current = home.0.join(".tokn/ctl/current");
  let installed = std::fs::canonicalize(&current).unwrap();
  let source = AgentSource::open(&installed).unwrap();
  assert_eq!(
    source.source_bundle.bundle_id,
    replacement.manifest.bundle_id
  );
  assert!(
    !installed.join(MANIFEST_FILE).exists(),
    "partial installs must not claim a full bundle"
  );
  for name in ["ctmuxd", "ctl-taskd", "ctld", "ctl", "ctmux"] {
    assert_eq!(
      std::fs::canonicalize(current.join(name)).unwrap(),
      std::fs::canonicalize(original.directory.join(name)).unwrap()
    );
    assert_eq!(
      std::fs::read(current.join(name)).unwrap(),
      std::fs::read(original.directory.join(name)).unwrap()
    );
  }
  crate::ssh_install::install_local_agent(&home.0, None, &archive)
    .await
    .unwrap();
  for name in ["ctmuxd", "ctl-taskd", "ctld", "ctl", "ctmux"] {
    assert_eq!(
      std::fs::canonicalize(current.join(name)).unwrap(),
      std::fs::canonicalize(original.directory.join(name)).unwrap()
    );
  }
  assert!(AgentSource::open(&std::fs::canonicalize(&current).unwrap()).is_ok());
  assert!(!home.0.join(".tokn/ctl/components/.sync-lock").exists());
}

#[tokio::test]
async fn failed_agent_activation_keeps_the_previous_installation() {
  let home = Home::new();
  let source = bundle(&home.0, 'a');
  let archive = agent_archive(&source).unwrap();
  crate::ssh_install::install_local_agent(&home.0, None, &archive)
    .await
    .unwrap();
  let current = home.0.join(".tokn/ctl/current");
  let previous = std::fs::read_link(&current).unwrap();
  let invalid_companions = home.0.join("invalid-companions");
  std::fs::create_dir(&invalid_companions).unwrap();
  std::fs::write(invalid_companions.join("ctmuxd"), b"not executable").unwrap();
  std::fs::set_permissions(
    invalid_companions.join("ctmuxd"),
    std::fs::Permissions::from_mode(0o600),
  )
  .unwrap();
  assert!(
    crate::ssh_install::install_local_agent(&home.0, Some(&invalid_companions), &archive)
      .await
      .is_err()
  );
  assert_eq!(std::fs::read_link(current).unwrap(), previous);
  assert!(!home.0.join(".tokn/ctl/components/.sync-lock").exists());
}

#[tokio::test]
async fn selected_source_stays_pinned_and_provided_source_does_not_change_it() {
  let home = Home::new();
  let old = bundle(&home.0, 'a');
  let new = bundle(&home.0, 'b');
  // Upload selection supports portable targets; use native local selection on
  // Linux, or its corresponding upload profile on macOS without signed ctld.
  let purpose = if cfg!(target_os = "macos") {
    Purpose::Upload
  } else {
    Purpose::Local
  };
  Store::new(&home.0).select(purpose, &old).unwrap();
  let selected = prepare(
    &home.0,
    ctl_core::paths::native_target(),
    purpose,
    &BuildSource::Selected,
    &[],
  )
  .await
  .unwrap();
  assert_eq!(selected.manifest.bundle_id, old.manifest.bundle_id);
  let provided = BuildSource::Provided {
    path: new.directory.clone(),
    local_build: false,
    ctld_package: None,
  };
  assert_eq!(
    prepare(
      &home.0,
      ctl_core::paths::native_target(),
      purpose,
      &provided,
      &[]
    )
    .await
    .unwrap()
    .manifest
    .bundle_id,
    new.manifest.bundle_id
  );
  assert_eq!(
    Store::new(&home.0)
      .selected(purpose, ctl_core::paths::native_target())
      .unwrap()
      .unwrap()
      .manifest
      .bundle_id,
    old.manifest.bundle_id
  );
}

#[test]
fn agent_package_rejects_changed_source_bytes() {
  let home = Home::new();
  let source = bundle(&home.0, 'a');
  std::fs::write(source.directory.join("ctl-agent"), b"different binary").unwrap();
  assert!(agent_archive(&source).is_err());
}

#[tokio::test]
async fn full_install_after_agent_only_replaces_all_retained_companions() {
  let home = Home::new();
  let original = bundle(&home.0, 'a');
  let replacement = bundle(&home.0, 'b');
  crate::ssh_install::install_local_agent(
    &home.0,
    Some(&original.directory),
    &agent_archive(&replacement).unwrap(),
  )
  .await
  .unwrap();
  // Exercise the shared atomic full installer using fixtures, without selecting
  // a fake unsigned local helper or touching any running service.
  crate::ssh_install::install_local_bundle(&home.0, &replacement)
    .await
    .unwrap();
  let current = home.0.join(".tokn/ctl/current");
  assert_eq!(
    std::fs::canonicalize(&current).unwrap(),
    std::fs::canonicalize(&replacement.directory).unwrap()
  );
  assert!(current.join(MANIFEST_FILE).is_file());
  assert!(!current.join(SOURCE_FILE).exists());
  for name in ctl_core::bundle_layout::ALL {
    assert!(
      !std::fs::symlink_metadata(current.join(name))
        .unwrap()
        .file_type()
        .is_symlink()
    );
    assert_eq!(
      std::fs::read(current.join(name)).unwrap(),
      std::fs::read(replacement.directory.join(name)).unwrap()
    );
  }
}

#[tokio::test]
async fn wrong_target_provided_source_is_rejected_before_importing_or_selecting() {
  let source_home = Home::new();
  let account = Home::new();
  let source = bundle(&source_home.0, 'a');
  let other_target = if ctl_core::paths::native_target().contains("aarch64") {
    "x86_64-unknown-linux-musl"
  } else {
    "aarch64-unknown-linux-musl"
  };
  let provided = BuildSource::Provided {
    path: source.directory,
    local_build: false,
    ctld_package: None,
  };
  assert!(
    prepare(&account.0, other_target, Purpose::Upload, &provided, &[])
      .await
      .is_err()
  );
  assert!(!account.0.join(".tokn/ctl/components").exists());
}

#[tokio::test]
async fn provided_managed_archive_imports_verified_content_without_changing_selection() {
  let source_home = Home::new();
  let account = Home::new();
  let source = bundle(&source_home.0, 'a');
  let archive = source_home.0.join("provided build.tar.gz");
  std::fs::write(
    &archive,
    crate::components::package_bundle(&source).unwrap().archive,
  )
  .unwrap();
  let imported = prepare(
    &account.0,
    ctl_core::paths::native_target(),
    Purpose::Local,
    &BuildSource::Provided {
      path: archive,
      local_build: false,
      ctld_package: None,
    },
    &[],
  )
  .await
  .unwrap();
  assert_eq!(imported.manifest.bundle_id, source.manifest.bundle_id);
  assert_eq!(imported.read_files().unwrap(), source.read_files().unwrap());
  assert!(
    Store::new(&account.0)
      .selected(Purpose::Local, ctl_core::paths::native_target())
      .unwrap()
      .is_none()
  );
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[tokio::test]
async fn initial_local_selection_can_use_an_included_portable_linux_build() {
  let home = Home::new();
  let directory = home.0.join("included");
  std::fs::create_dir(&directory).unwrap();
  let target = publisher_target(ctl_core::paths::native_target(), Purpose::Local);
  let mut fixture = Fixture::new("0.1.0", &"a".repeat(40));
  fixture.inner["target_triple"] = serde_json::json!(&target);
  let archive = fixture.archive();
  std::fs::write(
    directory.join("bundle-set.json"),
    fixture.bind_archive(&archive),
  )
  .unwrap();
  std::fs::write(
    directory.join(
      fixture.outer["targets"][target.as_ref()]["archive"]
        .as_str()
        .unwrap(),
    ),
    archive,
  )
  .unwrap();
  let prepared = prepare(
    &home.0,
    ctl_core::paths::native_target(),
    Purpose::Local,
    &BuildSource::Selected,
    &[directory],
  )
  .await
  .unwrap();
  assert_eq!(prepared.manifest.target_triple, target);
  assert!(
    Store::new(&home.0)
      .selected(Purpose::Local, ctl_core::paths::native_target())
      .unwrap()
      .is_none()
  );
}

#[tokio::test]
async fn development_builder_errors_include_bounded_diagnostics_without_docker() {
  let mut command = tokio::process::Command::new("/bin/sh");
  command.args([
    "-c",
    "printf 'Docker fixture is unavailable\\n' >&2; printf 'must not enter JSON stdout'; exit 17",
  ]);
  let error = run_development_builder(command)
    .await
    .unwrap_err()
    .to_string();
  assert!(error.contains("17"));
  assert!(error.contains("Docker fixture is unavailable"));
  let mut bytes = vec![b'x'; MAX_BUILD_DIAGNOSTIC_BYTES * 2];
  bytes.extend_from_slice(b"\nfinal compiler cause");
  let tail = build_diagnostics(bytes.as_slice(), &mut tokio::io::sink())
    .await
    .unwrap();
  assert_eq!(tail.len(), MAX_BUILD_DIAGNOSTIC_BYTES);
  assert!(tail.ends_with("final compiler cause"));
}
