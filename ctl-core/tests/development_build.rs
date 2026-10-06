#![cfg(feature = "development")]

use ctl_core::development as development_build;

use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};

const TARGET: &str = "aarch64-apple-darwin";

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-development-build-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn manifest(&self, checkout: &str) -> PathBuf {
    let root = self.0.join(checkout);
    let manifest = root.join("ctl/cli");
    std::fs::create_dir_all(&manifest).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(manifest.join("Cargo.toml"), "[package]\n").unwrap();
    manifest
  }

  fn output(&self, target: &str) -> PathBuf {
    let output = self.0.join(target).join("debug/build/ctl-cli-fixture/out");
    std::fs::create_dir_all(&output).unwrap();
    output
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn checkpoint(target: &Path, repository: &Path) -> PathBuf {
  let digest = format!(
    "{:x}",
    Sha256::digest(repository.to_str().unwrap().as_bytes())
  );
  target.join("ctl-dev/helpers").join(&digest[..20])
}

#[test]
fn implicit_target_directory_is_captured_without_runtime_working_directory() {
  let fixture = Fixture::new();
  let manifest = fixture.manifest("checkout");
  let output = fixture.output("custom-target");
  let context = development_build::context(&manifest, Path::new("ctl/cli"), &output, TARGET)
    .unwrap()
    .unwrap();
  let repository = fixture.0.join("checkout").canonicalize().unwrap();
  assert_eq!(context.repository_root, repository);
  assert_eq!(
    context.checkpoints,
    vec![checkpoint(
      &fixture.0.join("custom-target").canonicalize().unwrap(),
      &repository
    )],
  );
  assert!(context.checkpoints.iter().all(|path| path.is_absolute()));
}

#[test]
fn explicit_target_layout_keeps_native_target_and_base_candidates() {
  let fixture = Fixture::new();
  let manifest = fixture.manifest("checkout");
  let output = fixture.output(&format!("custom-target/{TARGET}"));
  let context = development_build::context(&manifest, Path::new("ctl/cli"), &output, TARGET)
    .unwrap()
    .unwrap();
  let target = fixture.0.join("custom-target").canonicalize().unwrap();
  assert_eq!(
    context.checkpoints,
    vec![
      checkpoint(&target.join(TARGET), &context.repository_root),
      checkpoint(&target, &context.repository_root),
    ],
  );
}

#[test]
fn cli_and_gui_builds_share_one_checkout_selection() {
  let fixture = Fixture::new();
  let cli_manifest = fixture.manifest("checkout");
  let gui_manifest = fixture.0.join("checkout/apps/desktop/src-tauri");
  std::fs::create_dir_all(&gui_manifest).unwrap();
  std::fs::write(gui_manifest.join("Cargo.toml"), "[package]\n").unwrap();
  let output = fixture.output("shared-target");
  let cli = development_build::context(&cli_manifest, Path::new("ctl/cli"), &output, TARGET)
    .unwrap()
    .unwrap();
  let gui = development_build::context(
    &gui_manifest,
    Path::new("apps/desktop/src-tauri"),
    &output,
    TARGET,
  )
  .unwrap()
  .unwrap();
  assert_eq!(cli.repository_root, gui.repository_root);
  assert_eq!(cli.checkpoints, gui.checkpoints);
}

#[test]
fn checkouts_sharing_a_target_directory_have_distinct_checkpoints() {
  let fixture = Fixture::new();
  let first = fixture.manifest("first-worktree");
  let second = fixture.manifest("second-worktree");
  let output = fixture.output("shared-target");
  let first = development_build::context(&first, Path::new("ctl/cli"), &output, TARGET)
    .unwrap()
    .unwrap();
  let second = development_build::context(&second, Path::new("ctl/cli"), &output, TARGET)
    .unwrap()
    .unwrap();
  assert_ne!(first.repository_root, second.repository_root);
  assert_ne!(first.checkpoints, second.checkpoints);
}

#[test]
fn non_macos_and_packaged_sources_emit_no_checkout_provenance() {
  let fixture = Fixture::new();
  let manifest = fixture.manifest("checkout");
  let output = fixture.output("target");
  assert!(
    development_build::context(
      &manifest,
      Path::new("ctl/cli"),
      &output,
      "x86_64-unknown-linux-gnu"
    )
    .unwrap()
    .is_none()
  );
  let packaged = fixture.0.join("registry/ctl-cli-0.1.0");
  std::fs::create_dir_all(&packaged).unwrap();
  std::fs::write(packaged.join("Cargo.toml"), "[package]\n").unwrap();
  assert!(
    development_build::context(&packaged, Path::new("ctl/cli"), &output, TARGET)
      .unwrap()
      .is_none()
  );
  development_build::write(&output, None).unwrap();
  let generated = std::fs::read_to_string(output.join("development_ctld.rs")).unwrap();
  assert!(!generated.contains(fixture.0.to_str().unwrap()));
  assert!(generated.contains("debug_assertions"));
}
