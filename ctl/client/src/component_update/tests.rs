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
  Store::cache(home)
    .publish(&manifest, &fixture.files)
    .unwrap()
}

#[tokio::test]
async fn legacy_agent_overlay_retains_exact_companions_for_migration() {
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

fn write_included(directory: &Path, target: &str, revision: char) {
  std::fs::create_dir(directory).unwrap();
  let mut fixture = Fixture::new("0.1.0", &revision.to_string().repeat(40)).with_clients();
  fixture.inner["target_triple"] = serde_json::json!(target);
  let archive = fixture.archive();
  std::fs::write(
    directory.join("bundle-set.json"),
    fixture.bind_archive(&archive),
  )
  .unwrap();
  let name = fixture.outer["targets"][target]["archive"]
    .as_str()
    .unwrap();
  std::fs::write(directory.join(name), archive).unwrap();
}

#[tokio::test]
async fn selected_source_uses_included_build_even_when_an_older_native_build_is_active() {
  let home = Home::new();
  let old = bundle(&home.0, 'a');
  let installs = ctl_core::installations::Installations::new(&home.0);
  installs.activate(&old).unwrap();
  let directory = home.0.join("included");
  let native = ctl_core::paths::native_target();
  let target = publisher_target(native, Purpose::Local);
  write_included(&directory, &target, 'b');
  let included = prepare(
    &home.0,
    native,
    Purpose::Local,
    &BuildSource::Selected,
    &[directory],
  )
  .await
  .unwrap();
  assert_eq!(
    included.manifest.components["ctl-agent"]
      .build
      .source_revision
      .as_deref(),
    Some("b".repeat(40).as_str())
  );
  assert_ne!(included.manifest.bundle_id, old.manifest.bundle_id);
  assert_eq!(installs.active().unwrap().unwrap().manifest, old.manifest);
}

#[tokio::test]
async fn selected_source_never_uses_a_legacy_upload_pin_or_active_build_as_offline_fallback() {
  let home = Home::new();
  let old = bundle(&home.0, 'a');
  let installs = ctl_core::installations::Installations::new(&home.0);
  installs.activate(&old).unwrap();
  let store = Store::new(&home.0);
  let legacy = store
    .publish(&old.manifest, &old.read_files().unwrap())
    .unwrap();
  store.select(Purpose::Upload, &legacy).unwrap();
  for purpose in [Purpose::Local, Purpose::Upload] {
    assert!(
      prepare(
        &home.0,
        ctl_core::paths::native_target(),
        purpose,
        &BuildSource::Selected,
        &[]
      )
      .await
      .is_err()
    );
  }
  assert_eq!(installs.active().unwrap().unwrap().manifest, old.manifest);
}

#[tokio::test]
async fn provided_source_caches_verified_files_without_changing_the_active_build() {
  let home = Home::new();
  let old = bundle(&home.0, 'a');
  let new = bundle(&home.0, 'b');
  let installs = ctl_core::installations::Installations::new(&home.0);
  installs.activate(&old).unwrap();
  let provided = BuildSource::Provided {
    path: new.directory.clone(),
    local_build: false,
    ctld_package: None,
  };
  let prepared = prepare(
    &home.0,
    ctl_core::paths::native_target(),
    Purpose::Local,
    &provided,
    &[],
  )
  .await
  .unwrap();
  assert_eq!(prepared.manifest, new.manifest);
  assert_eq!(installs.active().unwrap().unwrap().manifest, old.manifest);
  assert!(
    Store::cache(&home.0)
      .selected(Purpose::Upload, ctl_core::paths::native_target())
      .unwrap()
      .is_none()
  );
}

#[tokio::test]
async fn agent_only_updates_are_rejected_without_changing_active_files() {
  let home = Home::new();
  let old = bundle(&home.0, 'a');
  let installs = ctl_core::installations::Installations::new(&home.0);
  installs.activate(&old).unwrap();
  let replacement = bundle(&home.0, 'b');
  let error = install_local(&home.0, &replacement, Package::CtlAgent)
    .await
    .unwrap_err();
  assert!(error.to_string().contains("agent-only"));
  assert_eq!(installs.active().unwrap().unwrap().manifest, old.manifest);
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
  // Remote archive activation accepts the verified flat native fixture and
  // shares the core transaction without provisioning a fake signed helper.
  let archive = crate::components::package_bundle(&replacement)
    .unwrap()
    .archive;
  let installed = crate::installation::install_archive(&home.0, &archive)
    .await
    .unwrap();
  let current = home.0.join(".tokn/ctl/current");
  assert_eq!(
    std::fs::canonicalize(&current).unwrap(),
    installed.directory.canonicalize().unwrap()
  );
  assert_eq!(installed.manifest, replacement.manifest);
  assert!(
    installed
      .directory
      .starts_with(home.0.join(".tokn/ctl/installs"))
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
  assert!(!account.0.join(".tokn/ctl/cache/components").exists());
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
async fn an_included_portable_linux_build_is_prepared_without_activation() {
  let home = Home::new();
  let directory = home.0.join("included");
  std::fs::create_dir(&directory).unwrap();
  let target = publisher_target(ctl_core::paths::native_target(), Purpose::Local);
  let mut fixture = Fixture::new("0.1.0", &"a".repeat(40)).with_clients();
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
  assert!(!home.0.join(".tokn/ctl/current").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn flat_local_macos_update_is_rejected_before_manager_provisioning_or_activation() {
  let home = Home::new();
  let old = bundle(&home.0, 'a');
  let installs = ctl_core::installations::Installations::new(&home.0);
  installs.activate(&old).unwrap();
  let replacement = bundle(&home.0, 'b');
  let error = install_local(&home.0, &replacement, Package::FullBundle)
    .await
    .unwrap_err();
  assert!(error.to_string().contains("signed helper receipt"));
  assert_eq!(installs.active().unwrap().unwrap().manifest, old.manifest);
  assert!(!home.0.join(".tokn/ctl/bin/ctlup").exists());
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
