//! Per-user installation paths for the signed macOS connection broker.

use std::path::{Path, PathBuf};

/// Returns the installation directory without inspecting the filesystem.
#[must_use]
pub fn component_directory(home: &Path) -> PathBuf {
  home.join(".tokn/ctl/components/ctld")
}

/// Returns the selected bundle's executable without resolving `current`.
#[must_use]
pub fn executable(home: &Path) -> PathBuf {
  component_directory(home).join("current/ctld.app/Contents/MacOS/ctld")
}

#[cfg(unix)]
pub use filesystem::{
  ensure_component_directory, resolve_candidate_executable, resolve_executable,
  validate_current_selection,
};

#[cfg(unix)]
mod filesystem {
  use std::fs;
  use std::io;
  use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
  use std::path::{Component, Path, PathBuf};

  const ANCESTORS: [&str; 4] = [".tokn", "ctl", "components", "ctld"];

  /// Creates private managed directories without changing existing permissions.
  ///
  /// # Errors
  /// Returns an error if a directory is a symlink, belongs to another user, or
  /// can be modified by another user. Existing `ctld` and `versions`
  /// directories must grant access only to their owner.
  pub fn ensure_component_directory(home: &Path) -> io::Result<PathBuf> {
    let mut directory = checked_home(home)?;
    for name in ANCESTORS {
      directory.push(name);
      ensure_directory(&directory, name == "ctld")?;
    }
    ensure_directory(&directory.join("versions"), true)?;
    Ok(directory)
  }

  /// Validates and pins the selected managed helper to one installed version.
  ///
  /// A missing installation returns `None`. A malformed or incomplete
  /// installation returns an error so callers cannot silently launch an
  /// unsigned helper instead. Code signatures are verified when installing.
  ///
  /// # Errors
  /// Returns an error if the selection escapes `versions`, the bundle is
  /// incomplete, or any selected path is a symlink, belongs to another user,
  /// or can be modified by another user.
  pub fn resolve_executable(home: &Path) -> io::Result<Option<PathBuf>> {
    let directory = match checked_component_directory(home) {
      Ok(directory) => directory,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error),
    };
    let Some(selection) = checked_selection(&directory)? else {
      return Ok(None);
    };
    // Resolve the already selected version, never the mutable `current` link.
    validate_candidate(&directory, &selection).map(Some)
  }

  /// Checks whether the existing selection can safely be replaced by setup.
  ///
  /// Missing selections are allowed. A well-formed owned selection is allowed
  /// even when its old target is missing or incomplete, so setup can repair it.
  /// This function does not validate an executable or make it safe to launch.
  ///
  /// # Errors
  /// Returns an error for untrusted managed parent directories or a current
  /// selection that is foreign-owned, not a symlink, or escapes `versions`.
  pub fn validate_current_selection(home: &Path) -> io::Result<()> {
    let directory = match checked_component_directory(home) {
      Ok(directory) => directory,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
      Err(error) => return Err(error),
    };
    checked_selection(&directory).map(|_| ())
  }

  fn checked_selection(directory: &Path) -> io::Result<Option<PathBuf>> {
    let current = directory.join("current");
    let metadata = match fs::symlink_metadata(&current) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error),
    };
    if !metadata.file_type().is_symlink() || !owned(&metadata) {
      return Err(invalid_path(
        &current,
        "must be a symlink owned by this user",
      ));
    }
    let selection = fs::read_link(&current)?;
    if !valid_selection(&selection) {
      return Err(invalid_path(
        &current,
        "must point directly to a relative versions/<version>-<target> directory",
      ));
    }
    Ok(Some(selection))
  }

  /// Validates an unselected staged or cached helper before executing it.
  ///
  /// `candidate` must name either `versions/<version>-<target>` or
  /// `.setup-<name>/payload` directly inside the managed component directory.
  /// The returned executable is pinned to its canonical bundle path. Callers
  /// must verify its signature, notarization, and protocol before activation.
  ///
  /// # Errors
  /// Returns an error if the candidate escapes the managed component
  /// directory, uses symlinks, has untrusted ownership or permissions, or does
  /// not contain the complete signed and stapled bundle.
  pub fn resolve_candidate_executable(home: &Path, candidate: &Path) -> io::Result<PathBuf> {
    let directory = checked_component_directory(home)?;
    let relative = candidate
      .strip_prefix(&directory)
      .or_else(|_| candidate.strip_prefix(super::component_directory(home)))
      .map_err(|_| invalid_path(candidate, "must be inside the managed component directory"))?;
    validate_candidate(&directory, relative)
  }

  fn validate_candidate(directory: &Path, relative: &Path) -> io::Result<PathBuf> {
    let candidate = directory.join(relative);
    let components: Vec<_> = relative.components().collect();
    if relative.as_os_str() != components.iter().collect::<PathBuf>().as_os_str() {
      return Err(invalid_path(
        &candidate,
        "must use a direct relative candidate path",
      ));
    }
    match components.as_slice() {
      [Component::Normal(parent), Component::Normal(_)] if *parent == "versions" => {
        check_directory(&directory.join("versions"), true)?;
        check_directory(&candidate, false)?;
      }
      [Component::Normal(work), Component::Normal(payload)]
        if *payload == "payload"
          && work
            .to_str()
            .is_some_and(|name| name.starts_with(".setup-") && name.len() > 7) =>
      {
        check_directory(&directory.join(work), true)?;
        check_directory(&candidate, true)?;
      }
      _ => {
        return Err(invalid_path(
          &candidate,
          "must be versions/<version>-<target> or .setup-<name>/payload",
        ));
      }
    }
    validate_bundle(&candidate)
  }

  fn validate_bundle(version: &Path) -> io::Result<PathBuf> {
    let bundle = version.join("ctld.app");
    check_directory(&bundle, false)?;
    let contents = bundle.join("Contents");
    check_directory(&contents, false)?;
    check_directory(&contents.join("MacOS"), false)?;
    check_directory(&contents.join("_CodeSignature"), false)?;
    for resource in [
      "Info.plist",
      "embedded.provisionprofile",
      "_CodeSignature/CodeResources",
      "CodeResources",
    ] {
      check_file(&contents.join(resource), false)?;
    }
    let executable = contents.join("MacOS/ctld");
    check_file(&executable, true)?;
    executable.canonicalize()
  }

  fn checked_component_directory(home: &Path) -> io::Result<PathBuf> {
    let mut directory = checked_home(home)?;
    for name in ANCESTORS {
      directory.push(name);
      check_directory(&directory, name == "ctld")?;
    }
    Ok(directory)
  }

  fn checked_home(home: &Path) -> io::Result<PathBuf> {
    let home = home.canonicalize()?;
    check_directory(&home, false)?;
    Ok(home)
  }

  fn ensure_directory(path: &Path, private: bool) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
      Ok(()) => {}
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error),
    }
    check_directory(path, private)
  }

  fn check_directory(path: &Path, private: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || !owned(&metadata) || metadata.mode() & 0o022 != 0 {
      return Err(invalid_path(
        path,
        "must be a directory owned by this user without group or other write access",
      ));
    }
    if private && metadata.mode() & 0o777 != 0o700 {
      return Err(invalid_path(path, "must have permissions 0700"));
    }
    Ok(())
  }

  fn check_file(path: &Path, executable: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || !owned(&metadata) || metadata.mode() & 0o022 != 0 {
      return Err(invalid_path(
        path,
        "must be a regular file owned by this user without group or other write access",
      ));
    }
    if executable && metadata.mode() & 0o100 == 0 {
      return Err(invalid_path(path, "must be executable by its owner"));
    }
    Ok(())
  }

  fn owned(metadata: &fs::Metadata) -> bool {
    metadata.uid() == rustix::process::getuid().as_raw()
  }

  fn valid_selection(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(value)) if value == "versions")
      && matches!(components.next(), Some(Component::Normal(value)) if value != "." && value != "..")
      && components.next().is_none()
      // Components normalizes trailing separators and `.`; require the direct
      // relative spelling written by the installer, rather than accepting them.
      && path.as_os_str() == path.components().collect::<PathBuf>().as_os_str()
  }

  fn invalid_path(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "managed ctld path {} {reason}; run `ctl setup` to repair the installation",
        path.display()
      ),
    )
  }
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::fs;
  use std::io;
  use std::os::unix::fs::{PermissionsExt as _, symlink};
  use std::sync::atomic::{AtomicU64, Ordering};

  struct Fixture {
    home: PathBuf,
  }

  impl Fixture {
    fn new() -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(0);
      let home = std::env::temp_dir().join(format!(
        "ctld-managed-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
      ));
      fs::create_dir(&home).unwrap();
      fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
      Self { home }
    }

    fn install(&self, version: &str) -> PathBuf {
      let directory = ensure_component_directory(&self.home).unwrap();
      let bundle = directory.join("versions").join(version).join("ctld.app");
      let contents = bundle.join("Contents");
      fs::create_dir_all(contents.join("MacOS")).unwrap();
      fs::set_permissions(bundle.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
      fs::create_dir(contents.join("_CodeSignature")).unwrap();
      for resource in [
        "Info.plist",
        "embedded.provisionprofile",
        "_CodeSignature/CodeResources",
        "CodeResources",
        "MacOS/ctld",
      ] {
        fs::write(contents.join(resource), version).unwrap();
      }
      let executable = contents.join("MacOS/ctld");
      fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
      executable
    }

    fn select(&self, target: &str) {
      let current = component_directory(&self.home).join("current");
      let _ = fs::remove_file(&current);
      symlink(target, current).unwrap();
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.home);
    }
  }

  #[test]
  fn preserves_remote_install_layout_and_creates_private_component_directories() {
    let fixture = Fixture::new();
    assert_eq!(
      component_directory(&fixture.home),
      fixture.home.join(".tokn/ctl/components/ctld")
    );
    assert_eq!(
      executable(&fixture.home),
      fixture
        .home
        .join(".tokn/ctl/components/ctld/current/ctld.app/Contents/MacOS/ctld")
    );
    let existing = fixture.home.join(".tokn/ctl");
    fs::create_dir_all(&existing).unwrap();
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(existing.join("versions")).unwrap();
    fs::write(existing.join("versions/remote-marker"), "remote").unwrap();
    let directory = ensure_component_directory(&fixture.home).unwrap();
    for path in [&directory, &directory.join("versions")] {
      assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o700
      );
    }
    assert_eq!(
      fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
      0o755
    );
    assert_eq!(
      fs::read_to_string(existing.join("versions/remote-marker")).unwrap(),
      "remote"
    );
  }

  #[test]
  fn missing_selection_is_absent_but_broken_selection_is_an_error() {
    let fixture = Fixture::new();
    assert_eq!(resolve_executable(&fixture.home).unwrap(), None);
    validate_current_selection(&fixture.home).unwrap();
    ensure_component_directory(&fixture.home).unwrap();
    assert_eq!(resolve_executable(&fixture.home).unwrap(), None);
    validate_current_selection(&fixture.home).unwrap();
    fixture.select("versions/0.1.0-aarch64-apple-darwin");
    validate_current_selection(&fixture.home).unwrap();
    assert_eq!(
      resolve_executable(&fixture.home).unwrap_err().kind(),
      io::ErrorKind::NotFound
    );
  }

  #[test]
  fn setup_can_repair_owned_broken_selections_but_rejects_unsafe_links() {
    let fixture = Fixture::new();
    let selected = fixture.install("0.1.0-aarch64-apple-darwin");
    fixture.select("versions/0.1.0-aarch64-apple-darwin");
    fs::remove_file(&selected).unwrap();
    validate_current_selection(&fixture.home).unwrap();
    assert_eq!(
      resolve_executable(&fixture.home).unwrap_err().kind(),
      io::ErrorKind::NotFound
    );
    fixture.select("../outside");
    assert_eq!(
      validate_current_selection(&fixture.home)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
    let current = component_directory(&fixture.home).join("current");
    fs::remove_file(&current).unwrap();
    fs::write(&current, "versions/0.1.0-aarch64-apple-darwin").unwrap();
    assert_eq!(
      validate_current_selection(&fixture.home)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
  }

  #[test]
  fn pins_one_complete_version_when_current_changes() {
    let fixture = Fixture::new();
    let first = fixture.install("0.1.0-aarch64-apple-darwin");
    let second = fixture.install("0.2.0-aarch64-apple-darwin");
    fixture.select("versions/0.1.0-aarch64-apple-darwin");
    let selected = resolve_executable(&fixture.home).unwrap().unwrap();
    fixture.select("versions/0.2.0-aarch64-apple-darwin");
    assert_eq!(selected, first.canonicalize().unwrap());
    assert_eq!(
      fs::read_to_string(selected).unwrap(),
      "0.1.0-aarch64-apple-darwin"
    );
    assert_eq!(
      resolve_executable(&fixture.home).unwrap(),
      Some(second.canonicalize().unwrap())
    );
  }

  #[test]
  fn validates_cached_and_staged_candidates_before_any_selection() {
    let fixture = Fixture::new();
    let cached = fixture.install("0.1.0-aarch64-apple-darwin");
    let root = component_directory(&fixture.home);
    let cached_root = root.join("versions/0.1.0-aarch64-apple-darwin");
    assert_eq!(
      resolve_candidate_executable(&fixture.home, &cached_root).unwrap(),
      cached.canonicalize().unwrap()
    );
    fixture.install("staged");
    let work = root.join(".setup-fixture");
    fs::create_dir(&work).unwrap();
    fs::set_permissions(&work, fs::Permissions::from_mode(0o700)).unwrap();
    let staged_root = work.join("payload");
    fs::rename(root.join("versions/staged"), &staged_root).unwrap();
    let staged = resolve_candidate_executable(&fixture.home, &staged_root).unwrap();
    assert_eq!(
      staged,
      staged_root
        .join("ctld.app/Contents/MacOS/ctld")
        .canonicalize()
        .unwrap()
    );
    assert_eq!(resolve_executable(&fixture.home).unwrap(), None);

    for candidate in [
      fixture.home.join("outside"),
      cached_root.join("../0.1.0-aarch64-apple-darwin"),
      staged_root.join("ctld.app"),
      root.join("unmanaged/payload"),
    ] {
      assert_eq!(
        resolve_candidate_executable(&fixture.home, &candidate)
          .unwrap_err()
          .kind(),
        io::ErrorKind::PermissionDenied,
        "{}",
        candidate.display()
      );
    }
    let ticket = staged_root.join("ctld.app/Contents/CodeResources");
    fs::remove_file(&ticket).unwrap();
    assert_eq!(
      resolve_candidate_executable(&fixture.home, &staged_root)
        .unwrap_err()
        .kind(),
      io::ErrorKind::NotFound
    );
    symlink(cached_root.join("ctld.app/Contents/CodeResources"), &ticket).unwrap();
    assert_eq!(
      resolve_candidate_executable(&fixture.home, &staged_root)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
  }

  #[test]
  fn rejects_symlinked_or_nonprivate_staging_parents_before_bundle_checks() {
    let fixture = Fixture::new();
    fixture.install("cached");
    let root = component_directory(&fixture.home);
    let work = root.join(".setup-fixture");
    fs::create_dir(&work).unwrap();
    fs::set_permissions(&work, fs::Permissions::from_mode(0o700)).unwrap();
    let payload = work.join("payload");
    fs::rename(root.join("versions/cached"), &payload).unwrap();
    fs::set_permissions(&work, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
      resolve_candidate_executable(&fixture.home, &payload)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
    fs::set_permissions(&work, fs::Permissions::from_mode(0o700)).unwrap();
    let actual = root.join("other");
    fs::rename(&work, &actual).unwrap();
    symlink(&actual, &work).unwrap();
    assert_eq!(
      resolve_candidate_executable(&fixture.home, &payload)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
    assert_eq!(resolve_executable(&fixture.home).unwrap(), None);
  }

  #[test]
  fn rejects_escaping_or_indirect_selection_paths() {
    let fixture = Fixture::new();
    fixture.install("0.1.0-aarch64-apple-darwin");
    for target in [
      "/tmp/ctld",
      "../ctld",
      "versions/../ctld",
      "versions/0.1.0-aarch64-apple-darwin/ctld.app",
      "versions/0.1.0-aarch64-apple-darwin/",
      "versions/./0.1.0-aarch64-apple-darwin",
    ] {
      fixture.select(target);
      assert_eq!(
        resolve_executable(&fixture.home).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied,
        "{target}"
      );
    }
  }

  #[test]
  fn rejects_untrusted_parent_directories_without_repairing_them() {
    let fixture = Fixture::new();
    let directory = ensure_component_directory(&fixture.home).unwrap();
    let permissions = fs::Permissions::from_mode(0o755);
    fs::set_permissions(&directory, permissions).unwrap();
    assert_eq!(
      ensure_component_directory(&fixture.home)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
    assert_eq!(
      fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
      0o755
    );
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let ancestor = fixture.home.join(".tokn");
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
      ensure_component_directory(&fixture.home)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
    let other = fixture.home.join("other");
    fs::rename(fixture.home.join(".tokn/ctl/components"), &other).unwrap();
    symlink(&other, fixture.home.join(".tokn/ctl/components")).unwrap();
    assert_eq!(
      ensure_component_directory(&fixture.home)
        .unwrap_err()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
    assert_eq!(
      resolve_executable(&fixture.home).unwrap_err().kind(),
      io::ErrorKind::PermissionDenied
    );
  }

  #[test]
  fn rejects_partial_bundles_symlinked_executables_and_writable_files() {
    let fixture = Fixture::new();
    let selected = fixture.install("0.1.0-aarch64-apple-darwin");
    fixture.select("versions/0.1.0-aarch64-apple-darwin");
    let resources = selected
      .parent()
      .unwrap()
      .parent()
      .unwrap()
      .join("_CodeSignature/CodeResources");
    fs::remove_file(&resources).unwrap();
    assert_eq!(
      resolve_executable(&fixture.home).unwrap_err().kind(),
      io::ErrorKind::NotFound
    );
    fs::write(&resources, "signature").unwrap();
    fs::set_permissions(&selected, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
      resolve_executable(&fixture.home).unwrap_err().kind(),
      io::ErrorKind::PermissionDenied
    );
    fs::set_permissions(&selected, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
      resolve_executable(&fixture.home).unwrap_err().kind(),
      io::ErrorKind::PermissionDenied
    );
    fs::remove_file(&selected).unwrap();
    symlink(&resources, &selected).unwrap();
    assert_eq!(
      resolve_executable(&fixture.home).unwrap_err().kind(),
      io::ErrorKind::PermissionDenied
    );
  }
}
