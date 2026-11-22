use super::*;
use crate::bundles::{Manifest, Source, Store};
use crate::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use crate::private_fs::options;
use crate::protocol::ProtocolVersion;
use std::collections::BTreeMap;
use std::fs;
use std::io::BufRead as _;
use std::os::unix::fs::{DirBuilderExt as _, symlink};
use std::process::{Command, Stdio};

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-installs-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Self(path)
  }
}
impl Drop for Home {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn fixture(home: &Path, byte: u8, clients: bool) -> Bundle {
  let build = ComponentBuildInfo {
    version: "0.1.2".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "b".repeat(64),
    dirty: false,
  };
  let names = if clients {
    &crate::bundle_layout::ALL[..]
  } else {
    &crate::bundle_layout::SERVICES[..]
  };
  let components = names
    .iter()
    .map(|&name| {
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
          .map(|name| {
            ProtocolInfo::new(
              name,
              1,
              ProtocolVersion::new(1, 0, 1),
              &[ProtocolVersion::new(1, 0, 1)],
            )
          })
          .collect(),
        },
      )
    })
    .collect();
  let files: BTreeMap<String, Vec<u8>> = names
    .iter()
    .map(|&name| (name.into(), vec![byte; 8]))
    .collect();
  let manifest = Manifest::new(
    crate::paths::native_target(),
    Source::Ci,
    components,
    &files,
  )
  .unwrap();
  let inputs = home.join("fixture-inputs");
  if !inputs.exists() {
    crate::private_fs::create_directory(&inputs).unwrap();
  }
  Store::new(&inputs).publish(&manifest, &files).unwrap()
}

#[test]
fn status_is_passive_and_missing_installations_are_distinct() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  assert!(installs.active().unwrap().is_none());
  assert!(installs.previous().unwrap().is_none());
  assert!(installs.list().unwrap().is_empty());
  assert_eq!(
    installs.get(&"a".repeat(64)).unwrap_err().kind(),
    io::ErrorKind::NotFound
  );
  assert!(!home.0.join(".tokn").exists());
}

#[test]
fn activation_switches_stable_commands_without_keeping_a_previous_build() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = fixture(&home.0, 1, true);
  let first = installs.activate(&first).unwrap();
  assert!(first.directory.starts_with(installs.runtime_root()));
  assert!(installs.previous().unwrap().is_none());
  installs
    .install_manager(
      b"verified fixture launcher",
      &super::manager_build_fixture(),
      None,
    )
    .unwrap();
  for name in crate::bundle_layout::ALL.into_iter().chain(["ctlup"]) {
    assert_eq!(
      fs::read_link(installs.root().join("bin").join(name)).unwrap(),
      Path::new(".ctl-launcher")
    );
    assert_eq!(
      fs::read(installs.root().join("bin").join(name)).unwrap(),
      b"verified fixture launcher"
    );
  }
  let second = installs.activate(&fixture(&home.0, 2, true)).unwrap();
  assert!(installs.previous().unwrap().is_none());
  assert_eq!(
    installs.active().unwrap().unwrap().manifest,
    second.manifest
  );
  // Repeating an installation must keep commands on the selected build.
  installs.activate(&second).unwrap();
  assert!(installs.previous().unwrap().is_none());
  assert_eq!(
    fs::read(
      installs
        .active()
        .unwrap()
        .unwrap()
        .executable("ctl")
        .unwrap()
    )
    .unwrap(),
    vec![2; 8]
  );
  assert_eq!(
    installs.prune().unwrap().removed,
    [first.manifest.bundle_id]
  );
}

#[test]
fn leases_retain_old_payloads_until_their_users_release_them() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let (first, lease) = installs
    .activate_and_lease(&fixture(&home.0, 1, true))
    .unwrap();
  installs.activate(&fixture(&home.0, 2, true)).unwrap();
  installs.activate(&fixture(&home.0, 3, true)).unwrap();
  let result = installs.prune().unwrap();
  assert_eq!(
    result.retained_in_use,
    vec![first.manifest.bundle_id.clone()]
  );
  assert!(first.directory.exists());
  drop(lease);
  assert_eq!(
    installs.prune().unwrap().removed,
    vec![first.manifest.bundle_id]
  );
  assert!(!first.directory.exists());
  assert_eq!(installs.list().unwrap().len(), 1);
}

#[test]
fn crashed_process_leases_are_recovered_without_pid_or_age_checks() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let root = installs.runtime_leases().join(&first.manifest.bundle_id);
  fs::DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(&root)
    .unwrap();
  let path = root.join("crash-fixture");
  let file = options()
    .read(true)
    .write(true)
    .create_new(true)
    .open(path)
    .unwrap();
  file.lock().unwrap();
  let mut child = Command::new("sh")
    .args(["-c", "printf 'ready\\n' >&2; read -r ignored"])
    .stdin(Stdio::piped())
    .stdout(Stdio::from(file.try_clone().unwrap()))
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  let mut ready = String::new();
  std::io::BufReader::new(child.stderr.take().unwrap())
    .read_line(&mut ready)
    .unwrap();
  assert_eq!(ready, "ready\n");
  drop(file);
  installs.activate(&fixture(&home.0, 2, true)).unwrap();
  installs.activate(&fixture(&home.0, 3, true)).unwrap();
  assert_eq!(
    installs.prune().unwrap().retained_in_use,
    vec![first.manifest.bundle_id.clone()]
  );
  child.kill().unwrap();
  child.wait().unwrap();
  assert_eq!(
    installs.prune().unwrap().removed,
    vec![first.manifest.bundle_id]
  );
}

#[test]
fn collisions_unsafe_pointers_and_damaged_payloads_preserve_activation() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let command = installs.root().join("bin/ctl");
  fs::remove_file(&command).unwrap();
  fs::write(&command, b"user-owned command").unwrap();
  assert!(installs.activate(&fixture(&home.0, 2, true)).is_err());
  assert_eq!(installs.active().unwrap().unwrap().manifest, first.manifest);
  assert_eq!(fs::read(command).unwrap(), b"user-owned command");
  symlink("../../outside", installs.root().join("current")).unwrap();
  assert_eq!(installs.active().unwrap().unwrap().manifest, first.manifest);
  fs::write(first.directory.join("ctmuxd"), b"changed").unwrap();
  assert!(installs.active().is_err());
}

#[test]
fn prune_recovers_abandoned_staging_without_following_links() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let stage = installs
    .staging_root()
    .join(format!("runtime-{}", uuid::Uuid::new_v4()));
  fs::DirBuilder::new().mode(0o700).create(&stage).unwrap();
  fs::write(stage.join("partial-download"), b"incomplete").unwrap();
  installs.prune().unwrap();
  assert!(!stage.exists());
  let outside = home.0.join("unrelated");
  fs::DirBuilder::new().mode(0o700).create(&outside).unwrap();
  fs::write(outside.join("keep"), b"unrelated").unwrap();
  symlink(&outside, &stage).unwrap();
  assert!(installs.prune().is_err());
  assert_eq!(fs::read(outside.join("keep")).unwrap(), b"unrelated");
}

#[test]
fn deployment_cache_is_passive_and_checks_each_storage_parent() {
  let home = Home::new();
  let source = fixture(&home.0, 1, true);
  let cache = Store::cache(&home.0);
  let cached = cache
    .publish(&source.manifest, &source.read_files().unwrap())
    .unwrap();
  assert!(
    cached
      .directory
      .starts_with(home.0.join(".tokn/ctl/ctlup/cache/unpacked/bundles"))
  );
  assert!(Installations::new(&home.0).active().unwrap().is_none());
  assert!(!cache.root().join("selected").exists());

  let cache_path = home.0.join(".tokn/ctl/ctlup/cache");
  fs::remove_dir_all(&cache_path).unwrap();
  let outside = home.0.join("unrelated-cache");
  fs::DirBuilder::new().mode(0o700).create(&outside).unwrap();
  symlink(&outside, &cache_path).unwrap();
  assert!(
    cache
      .publish(&source.manifest, &source.read_files().unwrap())
      .is_err()
  );
  assert!(fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn cache_gc_waits_for_the_last_prepared_bundle_clone() {
  use std::time::{Duration, Instant};

  let home = Home::new();
  let first = fixture(&home.0, 1, true);
  let second = fixture(&home.0, 2, true);
  let cache = Store::cache(&home.0);
  let prepared = cache
    .publish(&first.manifest, &first.read_files().unwrap())
    .unwrap();
  let clone = prepared.clone();
  drop(prepared);
  drop(
    cache
      .publish(&second.manifest, &second.read_files().unwrap())
      .unwrap(),
  );
  let prune_until_removed = |pinned: Option<&str>| {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
      let result = cache.prune_cache(0).unwrap();
      if let Some(pinned) = pinned {
        assert!(
          result.retained_in_use.iter().any(|id| id == pinned),
          "{result:?}"
        );
      }
      // A parallel fork can hold an inherited lease briefly after our close.
      // A prune that removed anything must still pass the exact assertions.
      if !result.removed.is_empty() || Instant::now() >= deadline {
        break result;
      }
      std::thread::sleep(Duration::from_millis(10));
    }
  };
  let result = prune_until_removed(Some(&first.manifest.bundle_id));
  assert_eq!(result.removed, vec![second.manifest.bundle_id]);
  assert_eq!(
    result.retained_in_use,
    vec![first.manifest.bundle_id.clone()]
  );
  assert!(result.remaining_bytes > 0);
  assert!(clone.read_files().is_ok());
  drop(clone);
  let result = prune_until_removed(None);
  assert_eq!(result.removed, vec![first.manifest.bundle_id]);
  assert_eq!(result.remaining_bytes, 0);
  assert!(Store::new(&home.0).prune_cache(0).is_err());
}

#[test]
fn cache_gc_uses_access_order_and_inspection_does_not_touch_it() {
  let home = Home::new();
  let first = fixture(&home.0, 1, true);
  let second = fixture(&home.0, 2, true);
  let cache = Store::cache(&home.0);
  let cached = cache
    .publish(&first.manifest, &first.read_files().unwrap())
    .unwrap();
  let first_bytes: u64 = fs::read_dir(&cached.directory)
    .unwrap()
    .map(|entry| entry.unwrap().metadata().unwrap().len())
    .sum();
  drop(cached);
  cache
    .publish(&second.manifest, &second.read_files().unwrap())
    .unwrap();
  let lease_path = |id: &str| {
    cache
      .root()
      .join("leases")
      .join(crate::paths::native_target())
      .join(format!("{id}.lock"))
  };
  for id in [&first.manifest.bundle_id, &second.manifest.bundle_id] {
    let file = fs::OpenOptions::new()
      .write(true)
      .open(lease_path(id))
      .unwrap();
    file
      .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
      .unwrap();
  }
  cache.list(crate::paths::native_target()).unwrap();
  assert_eq!(
    fs::metadata(lease_path(&first.manifest.bundle_id))
      .unwrap()
      .modified()
      .unwrap(),
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)
  );
  cache
    .get(crate::paths::native_target(), &first.manifest.bundle_id)
    .unwrap();
  let result = cache.prune_cache(first_bytes).unwrap();
  assert_eq!(result.removed, vec![second.manifest.bundle_id]);
  assert_eq!(result.remaining_bytes, first_bytes);
  assert!(
    cache
      .get(crate::paths::native_target(), &first.manifest.bundle_id)
      .is_ok()
  );
}

#[test]
fn launching_an_active_build_sweeps_crash_records_but_keeps_live_users() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let active = installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let held = installs.lease(&active).unwrap();
  let root = installs.runtime_leases().join(&active.manifest.bundle_id);
  let stale = root.join("previous-crash");
  options().write(true).create_new(true).open(&stale).unwrap();
  let executable = active.executable("ctld").unwrap().canonicalize().unwrap();
  let next = leases::lease_executable(&executable).unwrap().unwrap();
  assert!(!stale.exists());
  assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
  drop(held);
  drop(next);
  assert_eq!(fs::read_dir(root).unwrap().count(), 0);
  fs::write(&executable, b"changed payload").unwrap();
  assert_eq!(
    leases::lease_executable(&executable).unwrap_err().kind(),
    io::ErrorKind::InvalidData
  );
}

#[cfg(target_os = "macos")]
#[test]
fn flat_remote_brokers_do_not_override_local_helpers_and_partial_apps_are_errors() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let flat = fixture(&home.0, 1, true);
  installs.activate(&flat).unwrap();
  let required = &[("ctld", &[ProtocolVersion::new(9, 0, 1)][..])];
  assert!(
    crate::bundles::selected_executable_at(&home.0, "ctld", required)
      .unwrap()
      .is_none()
  );
  let mut files = flat.read_files().unwrap();
  files.insert("ctld.app/Contents/MacOS/ctld".into(), files["ctld"].clone());
  let manifest = Manifest::new(
    crate::paths::native_target(),
    Source::Ci,
    flat.manifest.components,
    &files,
  )
  .unwrap();
  let partial = Store::cache(&home.0).publish(&manifest, &files).unwrap();
  installs.activate(&partial).unwrap();
  assert!(
    crate::bundles::selected_executable_at(&home.0, "ctld", &[])
      .unwrap_err()
      .to_string()
      .contains("receipt")
  );
}

#[test]
fn registry_failure_never_switches_the_selected_payload() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let next = fixture(&home.0, 2, true);
  let registry = installs.state_root().join("installations.json");
  let saved = installs.state_root().join("saved-installations.json");
  fs::rename(&registry, &saved).unwrap();
  symlink(&saved, &registry).unwrap();
  assert!(installs.activate(&next).is_err());
  assert!(installs.prune().is_err());
  assert!(first.directory.exists());
  fs::remove_file(&registry).unwrap();
  fs::rename(saved, registry).unwrap();
  assert_eq!(installs.active().unwrap().unwrap().manifest, first.manifest);
  assert_eq!(installs.activate(&next).unwrap().manifest, next.manifest);
}

#[test]
fn desktop_and_updater_selections_share_the_registry_without_losing_each_other() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let runtime = installs.activate(&fixture(&home.0, 1, true)).unwrap();
  installs
    .install_manager(b"test launcher", &super::manager_build_fixture(), None)
    .unwrap();
  let (updater, updater_lease) = installs.manager_and_lease().unwrap().unwrap();
  let desktop = DesktopSelection {
    content_id: "d".repeat(64),
    source: DesktopSource::Channel {
      channel: "nightly".into(),
    },
  };
  let payload = installs.desktop_root().join(&desktop.content_id);
  installs
    .publish_desktop(None, &desktop, || {
      filesystem::ensure_directory(&payload)?;
      let app = payload.join("app");
      options().create_new(true).write(true).open(&app)?;
      Ok(())
    })
    .unwrap();
  let app = payload.join("app").canonicalize().unwrap();
  let app_lease = leases::lease_executable(&app).unwrap().unwrap();
  let next = DesktopSelection {
    content_id: "a".repeat(64),
    source: DesktopSource::Provided,
  };
  assert_eq!(
    installs
      .publish_desktop(None, &next, || -> io::Result<()> {
        panic!("stale publish called")
      })
      .unwrap_err()
      .kind(),
    io::ErrorKind::WouldBlock
  );
  installs
    .publish_desktop(Some(&desktop), &next, || {
      filesystem::ensure_directory(&installs.desktop_root().join(&next.content_id))
    })
    .unwrap();
  installs
    .install_manager(b"second updater", &super::manager_build_fixture(), None)
    .unwrap();
  assert_eq!(
    installs.active().unwrap().unwrap().manifest,
    runtime.manifest
  );
  assert_eq!(installs.desktop_selection().unwrap(), Some(next));
  assert_ne!(
    installs.active_manager_id().unwrap(),
    Some(crate::bundles::digest(b"test launcher"))
  );
  installs.prune_unused().unwrap();
  assert!(payload.exists() && updater.exists());
  drop(app_lease);
  drop(updater_lease);
  installs.prune_unused().unwrap();
  assert!(payload.exists() && !updater.exists());
  installs.prune_all(false).unwrap();
  assert!(!payload.exists() && !updater.exists());
}
