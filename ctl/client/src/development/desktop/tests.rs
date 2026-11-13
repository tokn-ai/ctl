use super::*;
use serde_json::json;

struct Output(PathBuf);

impl Output {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!(
      "ctlup-desktop-result-test-{}",
      uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn descriptor(&self, archive: &str) {
    std::fs::write(
      self.0.join("result.json"),
      serde_json::to_vec(&json!({
        "schema_version": 1,
        "app_version": "0.1.3",
        "bundle_id": format!("local.{}", "a".repeat(64)),
        "archive": archive,
      }))
      .unwrap(),
    )
    .unwrap();
  }
}

impl Drop for Output {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn windows_build_results_cannot_redirect_the_portable_archive() {
  let output = Output::new();
  output.descriptor("ctmux.zip");
  std::fs::write(output.0.join("ctmux.zip"), b"portable package").unwrap();
  assert_eq!(
    read_output(&output.0, "ctmux.zip").unwrap().archive,
    output.0.join("ctmux.zip")
  );
  for archive in [
    "../ctmux.zip",
    "ctmux.app.tar.gz",
    "ctmux.AppImage",
    "C:\\ctmux.zip",
  ] {
    output.descriptor(archive);
    assert!(read_output(&output.0, "ctmux.zip").is_err(), "{archive}");
  }
}

#[test]
fn local_desktop_packages_must_exist_and_be_regular_files() {
  let output = Output::new();
  output.descriptor("ctmux.zip");
  assert!(read_output(&output.0, "ctmux.zip").is_err());
  std::fs::create_dir(output.0.join("ctmux.zip")).unwrap();
  assert!(read_output(&output.0, "ctmux.zip").is_err());
  #[cfg(unix)]
  {
    std::fs::remove_dir(output.0.join("ctmux.zip")).unwrap();
    std::fs::write(output.0.join("outside.zip"), b"payload").unwrap();
    std::os::unix::fs::symlink("outside.zip", output.0.join("ctmux.zip")).unwrap();
    assert!(read_output(&output.0, "ctmux.zip").is_err());
  }
}
