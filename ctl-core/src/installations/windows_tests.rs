use super::Installations;
use crate::bundle_layout::{ALL, executable_name, required_protocols};
use crate::bundles::{Bundle, Manifest, Source, Store};
use crate::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use crate::protocol::ProtocolVersion;
use std::fs;
use std::path::{Path, PathBuf};

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-installs-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    Self(path)
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn bundle(home: &Path, byte: u8) -> Bundle {
  let target = crate::paths::native_target();
  let build = ComponentBuildInfo {
    version: "0.1.2".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "b".repeat(64),
    dirty: false,
  };
  let version = ProtocolVersion::new(1, 0, 1);
  let components = ALL
    .into_iter()
    .map(|name| {
      let protocols = required_protocols(name)
        .iter()
        .map(|&name| ProtocolInfo::new(name, 1, version, &[version]))
        .collect();
      (
        name.into(),
        ComponentInfo {
          build: build.clone(),
          protocols,
        },
      )
    })
    .collect();
  let files = ALL
    .into_iter()
    .map(|name| (executable_name(target, name).into_owned(), vec![byte]))
    .collect();
  let manifest = Manifest::new(target, Source::Ci, components, &files).unwrap();
  Store::cache(home).publish(&manifest, &files).unwrap()
}

#[test]
fn stable_launchers_follow_selection_while_a_lease_pins_a_superseded_payload() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  assert!(installs.active().unwrap().is_none());
  assert!(installs.previous().unwrap().is_none());
  assert!(!installs.root().exists());

  installs.install_manager(b"launcher").unwrap();
  let launcher = installs.root().join("bin/ctl.exe");
  let first = installs.activate(&bundle(&home.0, 1)).unwrap();
  let (selected, lease) = installs.active_and_lease().unwrap().unwrap();
  assert_eq!(selected.directory, first.directory);
  assert_eq!(
    selected.executable("ctl").unwrap(),
    first.directory.join("ctl.exe")
  );

  let second = installs.activate(&bundle(&home.0, 2)).unwrap();
  assert_eq!(installs.rollback().unwrap().directory, first.directory);
  assert_eq!(
    installs.active().unwrap().unwrap().directory,
    first.directory
  );
  assert_eq!(
    installs.previous().unwrap().unwrap().directory,
    second.directory
  );
  assert_eq!(installs.rollback().unwrap().directory, second.directory);
  installs.activate(&bundle(&home.0, 3)).unwrap();
  assert_eq!(
    installs.prune().unwrap().retained_in_use,
    std::slice::from_ref(&first.manifest.bundle_id)
  );
  assert!(first.directory.exists());
  drop(lease);
  assert_eq!(
    installs.prune().unwrap().removed,
    [first.manifest.bundle_id]
  );
  assert!(!first.directory.exists());

  installs.install_manager(b"new updater").unwrap();
  assert_eq!(fs::read(launcher).unwrap(), b"launcher");
}

#[test]
fn command_collision_cannot_change_active_selection() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  installs.install_manager(b"launcher").unwrap();
  let first = installs.activate(&bundle(&home.0, 1)).unwrap();
  let command = installs.root().join("bin/ctmux.exe");
  fs::write(&command, b"other command").unwrap();
  assert!(installs.activate(&bundle(&home.0, 2)).is_err());
  assert_eq!(
    installs.active().unwrap().unwrap().directory,
    first.directory
  );
  assert_eq!(fs::read(command).unwrap(), b"other command");
}

#[test]
fn a_running_manager_keeps_its_payload_when_a_new_manager_is_selected() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  assert!(installs.manager_and_lease().unwrap().is_none());
  assert!(!installs.root().exists());

  installs.install_manager(b"first manager").unwrap();
  let (old_path, lease) = installs.manager_and_lease().unwrap().unwrap();
  let direct = super::leases::lease_executable(&old_path).unwrap().unwrap();
  installs.install_manager(b"new manager").unwrap();
  let (active_path, _active_lease) = installs.manager_and_lease().unwrap().unwrap();
  assert_eq!(fs::read(&active_path).unwrap(), b"new manager");
  assert_eq!(
    fs::read(installs.root().join("bin/ctlup.exe")).unwrap(),
    b"first manager"
  );

  installs.prune().unwrap();
  assert_eq!(fs::read(&old_path).unwrap(), b"first manager");
  drop(lease);
  installs.prune().unwrap();
  assert!(old_path.exists());
  drop(direct);
  installs.prune().unwrap();
  assert!(!old_path.exists());
  assert!(active_path.exists());
}
