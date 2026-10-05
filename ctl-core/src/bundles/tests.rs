use super::*;
use crate::component::{ComponentBuildInfo, ProtocolInfo};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-complete-bundle-{}", uuid::Uuid::new_v4()));
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

fn fixture(source: Source, byte: u8) -> (Manifest, BTreeMap<String, Vec<u8>>) {
  let build = ComponentBuildInfo {
    version: "0.1.0".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "b".repeat(64),
    dirty: source == Source::Local,
  };
  let protocol = |name| {
    ProtocolInfo::new(
      name,
      1,
      ProtocolVersion::new(1, 0, 1),
      &[ProtocolVersion::new(1, 0, 1)],
    )
  };
  let components = COMPONENTS
    .into_iter()
    .map(|name| {
      (
        name.into(),
        ComponentInfo {
          build: build.clone(),
          protocols: [
            "ctld",
            "ctld_lifecycle",
            "ctld_helper",
            "ctmux",
            "ctmux_control",
            "task",
            "task_control",
            "ctl_identity",
            "ctl_maintenance",
            "ctl_remote_vpn",
          ]
          .into_iter()
          .map(protocol)
          .collect(),
        },
      )
    })
    .collect();
  let files = COMPONENTS
    .into_iter()
    .map(|name| (name.into(), vec![byte; 8]))
    .collect();
  let manifest = Manifest::new(crate::paths::native_target(), source, components, &files).unwrap();
  (manifest, files)
}

#[test]
fn absent_selection_is_passive_and_does_not_create_directories() {
  let home = Home::new();
  let store = Store::new(&home.0);
  assert!(
    store
      .selected(Purpose::Local, crate::paths::native_target())
      .unwrap()
      .is_none()
  );
  assert!(!home.0.join(".tokn").exists());
}

#[test]
fn imports_never_select_and_selection_changes_only_explicitly() {
  let home = Home::new();
  let store = Store::new(&home.0);
  let (first, files) = fixture(Source::Ci, 1);
  let first = store.publish(&first, &files).unwrap();
  assert!(
    store
      .selected(Purpose::Upload, crate::paths::native_target())
      .unwrap()
      .is_none()
  );
  store.select(Purpose::Upload, &first).unwrap();
  let (second, files) = fixture(Source::Release, 2);
  let second = store.publish(&second, &files).unwrap();
  assert_eq!(
    store
      .selected(Purpose::Upload, crate::paths::native_target())
      .unwrap()
      .unwrap()
      .manifest,
    first.manifest
  );
  store.select(Purpose::Upload, &second).unwrap();
  assert_eq!(
    store
      .selected(Purpose::Upload, crate::paths::native_target())
      .unwrap()
      .unwrap()
      .manifest,
    second.manifest
  );
  assert!(
    store
      .selected(Purpose::Local, crate::paths::native_target())
      .unwrap()
      .is_none()
  );
}

#[test]
fn publication_is_immutable_and_damaged_selections_fail_without_fallback() {
  let home = Home::new();
  let store = Store::new(&home.0);
  let (manifest, files) = fixture(Source::Ci, 1);
  let bundle = store.publish(&manifest, &files).unwrap();
  assert_eq!(
    store.publish(&manifest, &files).unwrap().directory,
    bundle.directory
  );
  store.select(Purpose::Upload, &bundle).unwrap();
  let (other, other_files) = fixture(Source::Ci, 2);
  store.publish(&other, &other_files).unwrap();
  std::fs::write(bundle.directory.join("ctmuxd"), b"changed").unwrap();
  assert!(
    store
      .selected(Purpose::Upload, crate::paths::native_target())
      .is_err()
  );
  assert!(store.publish(&manifest, &files).is_err());
}

#[test]
fn mixed_builds_missing_components_and_incompatible_companions_are_rejected() {
  let (manifest, files) = fixture(Source::Local, 1);
  for change in 0..5 {
    let mut components = manifest.components.clone();
    match change {
      0 => components.get_mut("ctld").unwrap().build.source_revision = Some("c".repeat(40)),
      1 => components.get_mut("ctld").unwrap().build.source_fingerprint = "c".repeat(64),
      2 => components.get_mut("ctld").unwrap().build.dirty = false,
      3 => {
        components.remove("ctld");
      }
      _ => components.get_mut("ctld").unwrap().protocols.clear(),
    }
    assert!(
      Manifest::new(
        crate::paths::native_target(),
        Source::Local,
        components,
        &files
      )
      .is_err()
    );
  }
  let mut missing = files;
  missing.remove("ctl-taskd");
  assert!(
    Manifest::new(
      crate::paths::native_target(),
      Source::Local,
      manifest.components,
      &missing
    )
    .is_err()
  );
}

#[test]
fn symlinked_store_selection_and_payload_are_rejected() {
  let home = Home::new();
  let store = Store::new(&home.0);
  let (manifest, files) = fixture(Source::Ci, 1);
  let bundle = store.publish(&manifest, &files).unwrap();
  store.select(Purpose::Upload, &bundle).unwrap();
  let selection = store
    .root()
    .join("selected")
    .join(format!("upload-{}.json", crate::paths::native_target()));
  let real = selection.with_extension("real");
  std::fs::rename(&selection, &real).unwrap();
  symlink(&real, &selection).unwrap();
  assert!(
    store
      .selected(Purpose::Upload, crate::paths::native_target())
      .is_err()
  );
  std::fs::remove_file(selection).unwrap();
  let executable = bundle.directory.join("ctmuxd");
  std::fs::remove_file(&executable).unwrap();
  symlink(bundle.directory.join("ctld"), executable).unwrap();
  assert!(Bundle::open(&bundle.directory).is_err());
  let components = store.root().to_owned();
  std::fs::rename(&components, components.with_extension("real")).unwrap();
  symlink(components.with_extension("real"), components).unwrap();
  assert!(
    store
      .selected(Purpose::Upload, crate::paths::native_target())
      .is_err()
  );
}

#[test]
fn changed_or_invalid_import_keeps_previous_selection_usable() {
  let home = Home::new();
  let store = Store::new(&home.0);
  let (manifest, files) = fixture(Source::Ci, 1);
  let bundle = store.publish(&manifest, &files).unwrap();
  store.select(Purpose::Upload, &bundle).unwrap();
  let mut changed = files;
  changed.insert("ctl-taskd".into(), b"other".to_vec());
  assert!(store.publish(&manifest, &changed).is_err());
  assert_eq!(
    store
      .selected(Purpose::Upload, crate::paths::native_target())
      .unwrap()
      .unwrap()
      .manifest,
    manifest
  );
  std::fs::set_permissions(
    bundle.directory.join("ctld"),
    std::fs::Permissions::from_mode(0o777),
  )
  .unwrap();
  assert!(
    store
      .selected(Purpose::Upload, crate::paths::native_target())
      .is_err()
  );
}

#[test]
fn bootstrap_never_overwrites_an_explicit_selection_and_lookup_is_bounded() {
  let home = Home::new();
  let store = Store::new(&home.0);
  let (manifest, files) = fixture(Source::Ci, 1);
  let first = store.publish(&manifest, &files).unwrap();
  let (manifest, files) = fixture(Source::Ci, 2);
  let second = store.publish(&manifest, &files).unwrap();
  assert_eq!(
    store
      .select_if_unset(Purpose::Upload, &first)
      .unwrap()
      .manifest,
    first.manifest
  );
  store.select(Purpose::Upload, &second).unwrap();
  assert_eq!(
    store
      .select_if_unset(Purpose::Upload, &first)
      .unwrap()
      .manifest,
    second.manifest
  );
  assert!(store.get("../escape", &first.manifest.bundle_id).is_err());
  assert!(
    store
      .get(crate::paths::native_target(), "../escape")
      .is_err()
  );
  assert_eq!(
    store
      .get(crate::paths::native_target(), &first.manifest.bundle_id)
      .unwrap()
      .manifest,
    first.manifest
  );
}

#[test]
fn duplicate_manifest_keys_and_missing_executable_permissions_are_rejected() {
  let (manifest, files) = fixture(Source::Ci, 1);
  let json = serde_json::to_string(&manifest).unwrap();
  let duplicate = json.replacen(
    "\"components\":{",
    &format!(
      "\"components\":{{\"ctl-agent\":{},",
      serde_json::to_string(&manifest.components["ctl-agent"]).unwrap()
    ),
    1,
  );
  assert!(serde_json::from_str::<Manifest>(&duplicate).is_err());
  let home = Home::new();
  let bundle = Store::new(&home.0).publish(&manifest, &files).unwrap();
  std::fs::set_permissions(
    bundle.directory.join("ctl-agent"),
    std::fs::Permissions::from_mode(0o600),
  )
  .unwrap();
  assert!(Bundle::open(&bundle.directory).is_err());
}

#[test]
fn local_discovery_follows_explicit_selections_and_checks_the_requested_contracts() {
  let home = Home::new();
  let store = Store::new(&home.0);
  let local = |byte| {
    let (manifest, mut files) = fixture(Source::Local, byte);
    if cfg!(target_os = "macos") {
      files.insert("ctld.app/Contents/MacOS/ctld".into(), vec![byte; 8]);
      files.insert("ctld-package.json".into(), b"receipt".to_vec());
    }
    let manifest = Manifest::new(
      crate::paths::native_target(),
      Source::Local,
      manifest.components,
      &files,
    )
    .unwrap();
    store.publish(&manifest, &files).unwrap()
  };
  let first = local(1);
  let second = local(2);
  store.select(Purpose::Local, &first).unwrap();
  let required = &[("ctmux", &[ProtocolVersion::new(1, 0, 1)][..])];
  assert_eq!(
    selected_executable_at(&home.0, "ctmuxd", required).unwrap(),
    first.executable("ctmuxd")
  );
  store.select(Purpose::Local, &second).unwrap();
  assert_eq!(
    selected_executable_at(&home.0, "ctmuxd", required).unwrap(),
    second.executable("ctmuxd")
  );
  assert!(
    selected_executable_at(
      &home.0,
      "ctmuxd",
      &[("ctmux", &[ProtocolVersion::new(2, 0, 1)])]
    )
    .is_err()
  );
}

#[cfg(target_env = "gnu")]
#[test]
fn a_native_gnu_client_can_select_one_portable_local_bundle() {
  let home = Home::new();
  let store = Store::new(&home.0);
  let (manifest, files) = fixture(Source::Ci, 1);
  let target = crate::paths::native_target().replace("-gnu", "-musl");
  let manifest = Manifest::new(&target, Source::Ci, manifest.components, &files).unwrap();
  let bundle = store.publish(&manifest, &files).unwrap();
  store.select(Purpose::Local, &bundle).unwrap();
  assert_eq!(
    store
      .selected(Purpose::Local, crate::paths::native_target())
      .unwrap()
      .unwrap()
      .manifest,
    manifest
  );
  assert_eq!(
    store
      .selected(Purpose::Local, &target)
      .unwrap()
      .unwrap()
      .manifest,
    manifest
  );
}

#[test]
fn undeclared_files_and_changed_metadata_cannot_be_repackaged() {
  let home = Home::new();
  let store = Store::new(&home.0);
  let (manifest, files) = fixture(Source::Ci, 1);
  let bundle = store.publish(&manifest, &files).unwrap();
  let extra = bundle.directory.join("unexpected");
  std::fs::write(&extra, b"extra").unwrap();
  assert!(Bundle::open(&bundle.directory).is_err());
  assert!(bundle.read_files().is_err());
  std::fs::remove_file(extra).unwrap();
  std::fs::write(bundle.directory.join(MANIFEST_FILE), b"changed metadata").unwrap();
  assert!(bundle.read_files().is_err());
}

#[test]
fn helper_package_executable_flags_are_part_of_its_identity() {
  let (manifest, mut files) = fixture(Source::Local, 1);
  files.insert("ctld.app/Contents/Helpers/check".into(), b"helper".to_vec());
  let manifest = Manifest::new(
    crate::paths::native_target(),
    Source::Local,
    manifest.components,
    &files,
  )
  .unwrap();
  let executable = manifest
    .clone()
    .with_executables(&["ctld.app/Contents/Helpers/check".into()])
    .unwrap();
  assert_ne!(executable.bundle_id, manifest.bundle_id);
  let home = Home::new();
  let bundle = Store::new(&home.0).publish(&executable, &files).unwrap();
  assert_ne!(
    std::fs::metadata(bundle.directory.join("ctld.app/Contents/Helpers/check"))
      .unwrap()
      .permissions()
      .mode()
      & 0o100,
    0
  );
}
