use super::tests::Fixture;
use super::*;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt as _, symlink};

const TARGET: &str = "aarch64-apple-darwin";

#[test]
fn replacement_preflight_is_passive_and_allows_dangling_targets_for_repair() {
  let fixture = Fixture::new();
  validate_compatible_selection(&fixture.home, TARGET).unwrap();
  assert!(!fixture.home.join(".tokn").exists());
  let directory = ensure_component_directory(&fixture.home).unwrap();
  validate_compatible_selection(&fixture.home, TARGET).unwrap();
  assert!(!directory.join("selected").exists());
  for reference in [
    "../versions/missing".to_owned(),
    format!("../development/{}", "a".repeat(64)),
  ] {
    compatibility_link(&fixture, TARGET, &reference);
    validate_compatible_selection(&fixture.home, TARGET).unwrap();
    assert!(!directory.join("versions/missing").exists());
    assert!(!directory.join("development").exists());
  }
  // Setup may also repair a complete directory whose bundle became incomplete.
  let executable = fixture.install("incomplete");
  fs::remove_file(executable).unwrap();
  compatibility_link(&fixture, TARGET, "../versions/incomplete");
  validate_compatible_selection(&fixture.home, TARGET).unwrap();
}

#[test]
fn replacement_preflight_rejects_malformed_links_regular_files_and_unsafe_parents() {
  let fixture = Fixture::new();
  let directory = ensure_component_directory(&fixture.home).unwrap();
  for reference in [
    "/tmp/ctld",
    "../../versions/release",
    "../development/short",
  ] {
    let selection = compatibility_link(&fixture, TARGET, reference);
    assert_eq!(
      validate_compatible_selection(&fixture.home, TARGET)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
    assert_eq!(fs::read_link(selection).unwrap(), Path::new(reference));
  }
  let selection = compatible_selection(&fixture.home, TARGET).unwrap();
  fs::remove_file(&selection).unwrap();
  fs::write(&selection, "../versions/release").unwrap();
  assert_eq!(
    validate_compatible_selection(&fixture.home, TARGET)
      .unwrap_err()
      .kind(),
    io::ErrorKind::PermissionDenied
  );
  assert_eq!(
    fs::read_to_string(&selection).unwrap(),
    "../versions/release"
  );
  compatibility_link(&fixture, TARGET, "../versions/missing");
  let selected = directory.join("selected");
  fs::set_permissions(&selected, fs::Permissions::from_mode(0o755)).unwrap();
  assert_eq!(
    validate_compatible_selection(&fixture.home, TARGET)
      .unwrap_err()
      .kind(),
    io::ErrorKind::PermissionDenied
  );
  assert_eq!(
    fs::metadata(selected).unwrap().permissions().mode() & 0o777,
    0o755
  );
}

fn install_development(fixture: &Fixture, digest: &str) -> PathBuf {
  fixture.install("development-fixture");
  let candidate = ensure_development_directory(&fixture.home)
    .unwrap()
    .join(digest);
  fs::rename(
    component_directory(&fixture.home).join("versions/development-fixture"),
    &candidate,
  )
  .unwrap();
  fs::remove_file(candidate.join("ctld.app/Contents/CodeResources")).unwrap();
  candidate
}

fn compatibility_link(fixture: &Fixture, target: &str, reference: &str) -> PathBuf {
  let path = compatible_selection(&fixture.home, target).unwrap();
  let selected = path.parent().unwrap();
  if !selected.exists() {
    fs::create_dir(selected).unwrap();
    fs::set_permissions(selected, fs::Permissions::from_mode(0o700)).unwrap();
  }
  let _ = fs::remove_file(&path);
  symlink(reference, &path).unwrap();
  path
}

#[test]
fn missing_selection_never_creates_directories_or_scans_caches() {
  let fixture = Fixture::new();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET).unwrap(),
    None
  );
  assert!(!fixture.home.join(".tokn").exists());
  let directory = component_directory(&fixture.home);
  let selection = compatible_selection(&fixture.home, TARGET).unwrap();
  assert_eq!(
    selection,
    directory.join("selected").join(format!(
      "{TARGET}-ctld{}-lifecycle{}-helper{}",
      crate::PROTOCOL_VERSION.major,
      crate::lifecycle::PROTOCOL_VERSION.major,
      crate::HELPER_API_VERSION.major
    ))
  );
  fixture.install("release");
  fixture.select("versions/release");
  install_development(&fixture, &"a".repeat(64));
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET).unwrap(),
    None
  );
  assert!(!directory.join("selected").exists());
  for invalid in ["", "../aarch64-apple-darwin", "x86_64-unknown-linux-gnu"] {
    assert_eq!(
      compatible_selection(&fixture.home, invalid)
        .unwrap_err()
        .kind(),
      io::ErrorKind::InvalidInput
    );
    assert_eq!(
      resolve_compatible_installation(&fixture.home, invalid)
        .unwrap_err()
        .kind(),
      io::ErrorKind::InvalidInput
    );
  }
}

#[test]
fn release_and_development_selection_preserves_desktop_current() {
  let fixture = Fixture::new();
  let root = component_directory(&fixture.home);
  fixture.install("desktop");
  fixture.select("versions/desktop");
  fixture.install("release");
  let release = root.join("versions/release");
  select_compatible_installation(&fixture.home, TARGET, &release, false).unwrap();
  let first = resolve_compatible_installation(&fixture.home, TARGET)
    .unwrap()
    .unwrap();
  assert_eq!(first.directory, release.canonicalize().unwrap());
  assert!(!first.development);
  assert_eq!(
    fs::read_link(compatible_selection(&fixture.home, TARGET).unwrap()).unwrap(),
    Path::new("../versions/release")
  );
  let development = install_development(&fixture, &"a".repeat(64));
  select_compatible_installation(&fixture.home, TARGET, &development, true).unwrap();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET)
      .unwrap()
      .unwrap(),
    CompatibleInstallation {
      directory: development.canonicalize().unwrap(),
      development: true,
    }
  );
  assert_eq!(
    fs::read_link(compatible_selection(&fixture.home, TARGET).unwrap()).unwrap(),
    Path::new("../development").join("a".repeat(64))
  );
  // Pinned results survive replacement of the compatibility index.
  assert_eq!(first.directory, release.canonicalize().unwrap());
  assert_eq!(
    fs::read_link(root.join("current")).unwrap(),
    Path::new("versions/desktop")
  );
  assert_eq!(fs::read_dir(root.join("selected")).unwrap().count(), 1);
}

#[test]
fn entries_are_independent_by_target_and_required_apis() {
  let fixture = Fixture::new();
  let root = component_directory(&fixture.home);
  fixture.install("arm");
  fixture.install("intel");
  let intel = "x86_64-apple-darwin";
  let arm_selection = compatibility_link(&fixture, TARGET, "../versions/arm");
  let old_api_selection = arm_selection.with_file_name(format!(
    "{TARGET}-ctld{}-lifecycle{}-helper0",
    crate::PROTOCOL_VERSION.major,
    crate::lifecycle::PROTOCOL_VERSION.major
  ));
  fs::rename(&arm_selection, &old_api_selection).unwrap();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET).unwrap(),
    None
  );
  select_compatible_installation(&fixture.home, TARGET, &root.join("versions/arm"), false).unwrap();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, intel).unwrap(),
    None
  );
  select_compatible_installation(&fixture.home, intel, &root.join("versions/intel"), false)
    .unwrap();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET)
      .unwrap()
      .unwrap()
      .directory,
    root.join("versions/arm").canonicalize().unwrap()
  );
  assert_eq!(
    resolve_compatible_installation(&fixture.home, intel)
      .unwrap()
      .unwrap()
      .directory,
    root.join("versions/intel").canonicalize().unwrap()
  );
  assert_eq!(
    fs::read_link(old_api_selection).unwrap(),
    Path::new("../versions/arm")
  );
}

#[test]
fn replacement_preserves_the_previous_link_on_validation_error() {
  let fixture = Fixture::new();
  let root = component_directory(&fixture.home);
  fixture.install("release");
  let release = root.join("versions/release");
  select_compatible_installation(&fixture.home, TARGET, &release, false).unwrap();
  let selection = compatible_selection(&fixture.home, TARGET).unwrap();
  let original = fs::read_link(&selection).unwrap();
  let development = install_development(&fixture, &"a".repeat(64));
  fixture.install("incomplete");
  fs::remove_file(root.join("versions/incomplete/ctld.app/Contents/Info.plist")).unwrap();
  let staging = root.join(".setup-fixture");
  fs::create_dir(&staging).unwrap();
  fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
  fixture.install("staged");
  fs::rename(root.join("versions/staged"), staging.join("payload")).unwrap();
  for (candidate, development) in [
    (root.join("versions/incomplete"), false),
    (root.join("versions/missing"), false),
    (development, false),
    (release.clone(), true),
    (staging.join("payload"), false),
    (fixture.home.join("outside"), false),
  ] {
    assert!(
      select_compatible_installation(&fixture.home, TARGET, &candidate, development).is_err()
    );
    assert_eq!(fs::read_link(&selection).unwrap(), original);
    assert_eq!(
      fs::read_dir(selection.parent().unwrap()).unwrap().count(),
      1
    );
  }
}

#[test]
fn removed_installations_are_absent_but_existing_incomplete_bundles_error() {
  let fixture = Fixture::new();
  let selected = fixture.install("release");
  compatibility_link(&fixture, TARGET, "../versions/release");
  fs::remove_file(&selected).unwrap();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET)
      .unwrap_err()
      .kind(),
    io::ErrorKind::NotFound
  );
  fs::remove_dir_all(component_directory(&fixture.home).join("versions/release")).unwrap();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET).unwrap(),
    None
  );
  let development = install_development(&fixture, &"a".repeat(64));
  compatibility_link(
    &fixture,
    TARGET,
    &format!("../development/{}", "a".repeat(64)),
  );
  fs::remove_dir_all(development).unwrap();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET).unwrap(),
    None
  );
}

#[test]
fn discovery_and_replacement_reject_corrupt_or_escaping_index_links() {
  let fixture = Fixture::new();
  fixture.install("release");
  let candidate = component_directory(&fixture.home).join("versions/release");
  for reference in [
    "/tmp/ctld",
    "../../versions/release",
    "../versions/../release",
    "../versions/release/ctld.app",
    "../versions/release/",
    "../versions/./release",
    "../development/short",
    "../development/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    "../.setup-fixture/payload",
  ] {
    let selection = compatibility_link(&fixture, TARGET, reference);
    assert_eq!(
      resolve_compatible_installation(&fixture.home, TARGET)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied,
      "{reference}"
    );
    assert!(select_compatible_installation(&fixture.home, TARGET, &candidate, false).is_err());
    assert_eq!(fs::read_link(selection).unwrap(), Path::new(reference));
  }
  let selection = compatible_selection(&fixture.home, TARGET).unwrap();
  fs::remove_file(&selection).unwrap();
  fs::write(&selection, "../versions/release").unwrap();
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET)
      .unwrap_err()
      .kind(),
    io::ErrorKind::PermissionDenied
  );
  assert!(select_compatible_installation(&fixture.home, TARGET, &candidate, false).is_err());
  assert_eq!(
    fs::read_to_string(selection).unwrap(),
    "../versions/release"
  );
}

#[test]
fn selections_reject_unsafe_permissions_foreign_links_and_special_files() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let executable = fixture.install("release");
  let root = component_directory(&fixture.home);
  let candidate = root.join("versions/release");
  let selection = compatibility_link(&fixture, TARGET, "../versions/release");
  for directory in [
    root.join("selected"),
    root.join("versions"),
    candidate.clone(),
  ] {
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
      resolve_compatible_installation(&fixture.home, TARGET)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
    assert!(select_compatible_installation(&fixture.home, TARGET, &candidate, false).is_err());
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
  }
  fs::set_permissions(&executable, fs::Permissions::from_mode(0o777)).unwrap();
  assert!(resolve_compatible_installation(&fixture.home, TARGET).is_err());
  fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

  let actual = root.join("actual-release");
  fs::rename(&candidate, &actual).unwrap();
  symlink(&actual, &candidate).unwrap();
  assert!(resolve_compatible_installation(&fixture.home, TARGET).is_err());
  assert!(select_compatible_installation(&fixture.home, TARGET, &candidate, false).is_err());
  fs::remove_file(&candidate).unwrap();
  fs::rename(&actual, &candidate).unwrap();
  let actual_selected = root.join("actual-selected");
  fs::rename(root.join("selected"), &actual_selected).unwrap();
  symlink(&actual_selected, root.join("selected")).unwrap();
  assert!(resolve_compatible_installation(&fixture.home, TARGET).is_err());
  assert!(select_compatible_installation(&fixture.home, TARGET, &candidate, false).is_err());
  fs::remove_file(root.join("selected")).unwrap();
  fs::rename(&actual_selected, root.join("selected")).unwrap();

  fs::remove_file(&selection).unwrap();
  assert!(
    std::process::Command::new("/usr/bin/mkfifo")
      .arg(&selection)
      .status()
      .unwrap()
      .success()
  );
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET)
      .unwrap_err()
      .kind(),
    io::ErrorKind::PermissionDenied
  );
  assert!(select_compatible_installation(&fixture.home, TARGET, &candidate, false).is_err());
  fs::remove_file(&selection).unwrap();
  compatibility_link(&fixture, TARGET, "../versions/release");
  fs::remove_file(&executable).unwrap();
  assert!(
    std::process::Command::new("/usr/bin/mkfifo")
      .arg(&executable)
      .status()
      .unwrap()
      .success()
  );
  assert_eq!(
    resolve_compatible_installation(&fixture.home, TARGET)
      .unwrap_err()
      .kind(),
    io::ErrorKind::PermissionDenied
  );
}
