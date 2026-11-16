use super::*;
use std::os::unix::fs::DirBuilderExt as _;

struct Scratch(PathBuf);

impl Drop for Scratch {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn identical_appimage_bytes_require_matching_executable_permissions() {
  let scratch = Scratch(std::env::temp_dir().join(format!("ctlup-image-{}", uuid::Uuid::new_v4())));
  fs::DirBuilder::new()
    .mode(0o700)
    .create(&scratch.0)
    .unwrap();
  let installed = scratch.0.join("installed.AppImage");
  let staged = scratch.0.join("staged.AppImage");
  let bytes = b"\x7fELF\x02\x01\x01\0AI\x02fixture";
  fs::write(&installed, bytes).unwrap();
  fs::write(&staged, bytes).unwrap();
  let staged = prepare(staged).unwrap();
  fs::set_permissions(&installed, Permissions::from_mode(0o644)).unwrap();

  assert!(!matches(&installed, &staged).unwrap());
  let installed = prepare(installed).unwrap();
  assert!(matches(&installed, &staged).unwrap());
}
