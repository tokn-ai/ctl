use super::*;
use crate::bundles::{Manifest, Source};
use crate::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
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
  Store::new(home).publish(&manifest, &files).unwrap()
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
fn activation_switches_one_build_and_rollback_preserves_both_payloads() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = fixture(&home.0, 1, true);
  let first = installs.activate(&first).unwrap();
  assert!(
    first
      .directory
      .starts_with(installs.root().join("installs"))
  );
  assert!(installs.previous().unwrap().is_none());
  for name in crate::bundle_layout::ALL {
    if name == "ctld" {
      assert_eq!(
        fs::read(installs.root().join("bin/ctld")).unwrap(),
        filesystem::CTLD_PROXY
      );
      continue;
    }
    assert_eq!(
      fs::read_link(installs.root().join("bin").join(name)).unwrap(),
      Path::new("../current").join(name)
    );
    assert_eq!(
      fs::read(installs.root().join("bin").join(name)).unwrap(),
      vec![1; 8]
    );
  }
  let second = installs.activate(&fixture(&home.0, 2, true)).unwrap();
  assert_eq!(
    installs.previous().unwrap().unwrap().manifest,
    first.manifest
  );
  assert_eq!(
    installs.active().unwrap().unwrap().manifest,
    second.manifest
  );
  // Repeating an installation must not destroy the rollback candidate.
  installs.activate(&second).unwrap();
  assert_eq!(
    installs.previous().unwrap().unwrap().manifest,
    first.manifest
  );
  assert_eq!(installs.rollback().unwrap().manifest, first.manifest);
  assert_eq!(
    installs.previous().unwrap().unwrap().manifest,
    second.manifest
  );
  assert_eq!(
    fs::read(installs.root().join("bin/ctl")).unwrap(),
    vec![1; 8]
  );
}

#[test]
fn old_complete_current_is_migrated_and_retained_for_rollback() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let old = fixture(&home.0, 1, false);
  let relative = old.directory.strip_prefix(installs.root()).unwrap();
  symlink(relative, installs.root().join("current")).unwrap();
  assert_eq!(installs.active().unwrap().unwrap().manifest, old.manifest);
  let next = fixture(&home.0, 2, true);
  installs.activate(&next).unwrap();
  let previous = installs.previous().unwrap().unwrap();
  assert_eq!(previous.manifest, old.manifest);
  assert!(
    previous
      .directory
      .starts_with(installs.root().join("installs"))
  );
  assert_eq!(installs.rollback().unwrap().manifest, old.manifest);
  // Historical service-only builds can be retained, but not newly installed.
  assert!(installs.activate(&old).is_err());
}

#[test]
fn old_local_selection_can_be_migrated_without_a_current_link() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = fixture(&home.0, 1, true);
  #[cfg(not(target_os = "macos"))]
  Store::new(&home.0).select(Purpose::Local, &first).unwrap();
  #[cfg(target_os = "macos")]
  {
    let selected = Store::new(&home.0).root().join("selected");
    fs::create_dir(&selected).unwrap();
    fs::set_permissions(
      &selected,
      std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let path = selected.join(format!("local-{}.json", crate::paths::native_target()));
    fs::write(
      &path,
      serde_json::to_vec(&serde_json::json!({
        "bundle_id": first.manifest.bundle_id, "target_triple": first.manifest.target_triple,
      }))
      .unwrap(),
    )
    .unwrap();
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
  }
  assert_eq!(installs.active().unwrap().unwrap().manifest, first.manifest);
  installs.activate(&first).unwrap();
  assert!(
    fs::read_link(installs.root().join("current"))
      .unwrap()
      .starts_with("installs")
  );
}

#[test]
fn leases_retain_old_payloads_until_their_users_release_them() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let lease = installs.lease(&first).unwrap();
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
  assert_eq!(installs.list().unwrap().len(), 2);
}

#[test]
fn crashed_process_leases_are_recovered_without_pid_or_age_checks() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let root = installs
    .root()
    .join("leases")
    .join(&first.manifest.bundle_id);
  fs::DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(&root)
    .unwrap();
  let path = root.join("crash-fixture");
  let file = filesystem::options()
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
  fs::remove_file(installs.root().join("current")).unwrap();
  symlink("../../outside", installs.root().join("current")).unwrap();
  assert!(installs.active().is_err());
  fs::remove_file(installs.root().join("current")).unwrap();
  symlink(
    Path::new("installs").join(&first.manifest.bundle_id),
    installs.root().join("current"),
  )
  .unwrap();
  fs::write(first.directory.join("ctmuxd"), b"changed").unwrap();
  assert!(installs.active().is_err());
}

#[test]
fn interrupted_activation_journal_completes_before_next_operation() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let first = installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let second = fixture(&home.0, 2, true);
  let second = filesystem::publish(&installs, &second, None).unwrap();
  let path = installs.root().join(".activation.json");
  fs::write(
    &path,
    serde_json::to_vec(&serde_json::json!({
      "active": second.manifest.bundle_id,
      "previous": first.manifest.bundle_id,
    }))
    .unwrap(),
  )
  .unwrap();
  fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
  installs.prune().unwrap();
  assert_eq!(
    installs.active().unwrap().unwrap().manifest,
    second.manifest
  );
  assert_eq!(
    installs.previous().unwrap().unwrap().manifest,
    first.manifest
  );
  assert!(!path.exists());
}

#[test]
fn prune_recovers_abandoned_staging_without_following_links() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  installs.activate(&fixture(&home.0, 1, true)).unwrap();
  let stage = installs
    .root()
    .join("installs")
    .join(format!(".partial-{}", uuid::Uuid::new_v4()));
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
      .starts_with(home.0.join(".tokn/ctl/cache/components/bundles"))
  );
  assert!(Installations::new(&home.0).active().unwrap().is_none());
  assert!(!cache.root().join("selected").exists());

  let cache_path = home.0.join(".tokn/ctl/cache");
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
fn original_unmanaged_versions_can_be_superseded_without_claiming_a_rollback() {
  let home = Home::new();
  let source = fixture(&home.0, 1, true);
  let installs = Installations::new(&home.0);
  let old = installs.root().join("versions/legacy");
  fs::DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(&old)
    .unwrap();
  fs::write(old.join("ctl-agent"), b"old agent").unwrap();
  symlink("versions/legacy", installs.root().join("current")).unwrap();
  assert!(installs.active().unwrap().is_none());
  installs.activate(&source).unwrap();
  assert!(installs.previous().unwrap().is_none());
  assert_eq!(fs::read(old.join("ctl-agent")).unwrap(), b"old agent");
  assert_eq!(
    installs.active().unwrap().unwrap().manifest,
    source.manifest
  );
}

#[test]
fn cache_gc_waits_for_the_last_prepared_bundle_clone() {
  let home = Home::new();
  let first = fixture(&home.0, 1, true);
  let second = fixture(&home.0, 2, true);
  let cache = Store::cache(&home.0);
  let prepared = cache
    .publish(&first.manifest, &first.read_files().unwrap())
    .unwrap();
  let clone = prepared.clone();
  drop(prepared);
  cache
    .publish(&second.manifest, &second.read_files().unwrap())
    .unwrap();
  let result = cache.prune_cache(0).unwrap();
  assert_eq!(result.removed, vec![second.manifest.bundle_id]);
  assert_eq!(
    result.retained_in_use,
    vec![first.manifest.bundle_id.clone()]
  );
  assert!(result.remaining_bytes > 0);
  assert!(clone.read_files().is_ok());
  drop(clone);
  let result = cache.prune_cache(0).unwrap();
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
  let root = installs
    .root()
    .join("leases")
    .join(&active.manifest.bundle_id);
  let stale = root.join("previous-crash");
  filesystem::options()
    .write(true)
    .create_new(true)
    .open(&stale)
    .unwrap();
  let next = installs.lease(&active).unwrap();
  assert!(!stale.exists());
  assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
  drop(held);
  drop(next);
  assert_eq!(fs::read_dir(root).unwrap().count(), 0);
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

fn broker_fixture(home: &Path, signed: bool) -> Bundle {
  let source = fixture(home, if signed { 2 } else { 1 }, true);
  let label = if signed { "signed" } else { "flat" };
  let bytes = format!("#!/bin/sh\nprintf '{label}\\n'\nprintf '<%s>\\n' \"$@\"\n").into_bytes();
  let mut files = source.read_files().unwrap();
  files.insert("ctld".into(), bytes.clone());
  if signed {
    files.insert("ctld.app/Contents/MacOS/ctld".into(), bytes);
    files.insert("ctld-package.json".into(), b"fixture receipt".to_vec());
  }
  let manifest = Manifest::new(
    crate::paths::native_target(),
    Source::Ci,
    source.manifest.components,
    &files,
  )
  .unwrap();
  Store::cache(home).publish(&manifest, &files).unwrap()
}

#[test]
fn broker_proxy_stays_constant_across_package_shapes_and_failed_activation() {
  use std::os::unix::fs::MetadataExt as _;
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let flat = installs.activate(&broker_fixture(&home.0, false)).unwrap();
  let command = installs.root().join("bin/ctld");
  let run = || {
    let output = Command::new(&command)
      .args(["one", "two words"])
      .output()
      .unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
  };
  assert_eq!(run(), "flat\n<one>\n<two words>\n");
  let inode = fs::metadata(&command).unwrap().ino();
  let proxy = fs::read(&command).unwrap();
  let signed = broker_fixture(&home.0, true);
  let signed = filesystem::publish(&installs, &signed, None).unwrap();
  // Fail journal publication after command preparation. The old current must
  // still work, including the different package shape of its broker.
  let journal = installs.root().join(".activation.json");
  fs::DirBuilder::new().mode(0o700).create(&journal).unwrap();
  assert!(filesystem::activate(&installs, &signed, Some(&flat)).is_err());
  assert_eq!(run(), "flat\n<one>\n<two words>\n");
  assert_eq!(fs::read(&command).unwrap(), proxy);
  fs::remove_dir(journal).unwrap();
  installs.activate(&signed).unwrap();
  assert_eq!(run(), "signed\n<one>\n<two words>\n");
  assert_eq!(fs::metadata(&command).unwrap().ino(), inode);
  installs.rollback().unwrap();
  assert_eq!(run(), "flat\n<one>\n<two words>\n");
  assert_eq!(fs::read(&command).unwrap(), proxy);
}

#[test]
fn broker_alias_migrates_but_an_unrecognized_regular_command_is_preserved() {
  let home = Home::new();
  let installs = Installations::new(&home.0);
  let source = broker_fixture(&home.0, false);
  installs.activate(&source).unwrap();
  let command = installs.root().join("bin/ctld");
  fs::remove_file(&command).unwrap();
  symlink("../current/ctld", &command).unwrap();
  installs.activate(&source).unwrap();
  assert_eq!(fs::read(&command).unwrap(), filesystem::CTLD_PROXY);
  fs::write(&command, b"user command").unwrap();
  let error = installs.activate(&source).unwrap_err();
  assert!(error.to_string().contains("managed proxy"));
  assert_eq!(fs::read(command).unwrap(), b"user command");
}
