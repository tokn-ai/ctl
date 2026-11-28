//! Prepare an immutable CI input in the signing job's private staging directory.

use crate::updater::app::macos;
use ctl_client::remote_bundle::desktop::DesktopSigning;
use ctl_core::private_fs;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn prepare(archive: &Path, staged: &Path, version: &str) -> io::Result<PathBuf> {
  let app = macos::stage(archive, staged, version, DesktopSigning::Unsigned)?;
  verify_contents(&app, version)?;
  Ok(app)
}

fn verify_contents(app: &Path, version: &str) -> io::Result<()> {
  // Internal resource symlinks are supported by the installer, but the signing
  // job must replace and seal the actual helper and executables at these paths.
  for directory in [
    "Contents",
    "Contents/MacOS",
    "Contents/Helpers",
    "Contents/Helpers/ctld.app",
    "Contents/Helpers/ctld.app/Contents",
    "Contents/Helpers/ctld.app/Contents/MacOS",
    "Contents/Resources",
    "Contents/Resources/resources",
    "Contents/Resources/resources/agent-bundles",
  ] {
    if !fs::symlink_metadata(app.join(directory))?.is_dir() {
      return Err(invalid(format!(
        "expected a real desktop bundle directory: {directory}"
      )));
    }
  }
  for executable in [
    "Contents/MacOS/ctmux-app",
    "Contents/MacOS/ctmuxd",
    "Contents/MacOS/ctl-taskd",
    "Contents/Helpers/ctld.app/Contents/MacOS/ctld",
  ] {
    let info = fs::symlink_metadata(app.join(executable))?;
    if !info.is_file() || info.len() == 0 || !private_fs::is_executable(&info) {
      return Err(invalid(format!("invalid desktop executable: {executable}")));
    }
  }
  let plist = app.join("Contents/Helpers/ctld.app/Contents/Info.plist");
  if !fs::symlink_metadata(&plist)?.is_file()
    || macos::plist_string(&plist, "CFBundleIdentifier")? != "dev.tokn-ai.ctl.ctld"
    || macos::plist_string(&plist, "CFBundleExecutable")? != "ctld"
    || macos::plist_string(&plist, "CFBundleShortVersionString")? != version
  {
    return Err(invalid("ctld helper identity differs from the release"));
  }
  let resource =
    fs::symlink_metadata(app.join("Contents/Resources/resources/agent-bundles/bundle-set.json"))?;
  if !resource.is_file() || resource.len() == 0 {
    return Err(invalid("missing desktop agent bundle set"));
  }
  Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs::File;
  use std::io::Write as _;
  use std::os::unix::fs::symlink;
  use std::process::Command;

  const VERSION: &str = "0.1.4";

  struct Fixture {
    root: PathBuf,
    archive: PathBuf,
  }

  impl Fixture {
    fn new(change: impl FnOnce(&Path)) -> Self {
      let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
      fs::create_dir_all(&output).unwrap();
      let root = output.join(format!("test-macos-signing-input-{}", uuid::Uuid::new_v4()));
      private_fs::create_directory(&root).unwrap();
      let app = root.join("ctmux.app");
      let contents = app.join("Contents");
      let helper = contents.join("Helpers/ctld.app/Contents");
      for (dir, executable, identifier) in [
        (&contents, "ctmux-app", "dev.tokn-ai.ctl.ctmux"),
        (&helper, "ctld", "dev.tokn-ai.ctl.ctld"),
      ] {
        fs::create_dir_all(dir.join("MacOS")).unwrap();
        let mut binary = File::create(dir.join("MacOS").join(executable)).unwrap();
        binary.write_all(b"fixture").unwrap();
        private_fs::set_executable(&binary).unwrap();
        fs::write(
          dir.join("Info.plist"),
          format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleExecutable</key><string>{executable}</string>
<key>CFBundleShortVersionString</key><string>{VERSION}</string></dict></plist>"#
          ),
        )
        .unwrap();
      }
      for name in ["ctmuxd", "ctl-taskd"] {
        fs::copy(
          contents.join("MacOS/ctmux-app"),
          contents.join("MacOS").join(name),
        )
        .unwrap();
      }
      let resources = contents.join("Resources/resources/agent-bundles");
      fs::create_dir_all(&resources).unwrap();
      fs::write(resources.join("bundle-set.json"), "{}").unwrap();
      change(&app);
      let archive = root.join("desktop.app.tar.gz");
      let tar = Command::new("/usr/bin/tar")
        .arg("-czf")
        .arg(&archive)
        .args(["--mac-metadata", "-C"])
        .arg(&root)
        .arg("ctmux.app")
        .output()
        .unwrap();
      assert!(tar.status.success(), "{tar:?}");

      Self { root, archive }
    }

    fn prepare(&self) -> io::Result<PathBuf> {
      let staged = self.root.join("bundle");
      private_fs::create_directory(&staged)?;
      prepare(&self.archive, &staged, VERSION)
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      fs::remove_dir_all(&self.root).unwrap();
    }
  }

  #[test]
  fn extracts_without_changing_the_local_archive() {
    let fixture = Fixture::new(|app| {
      symlink("MacOS/ctmux-app", app.join("Contents/Current")).unwrap();
    });
    let archive = fs::read(&fixture.archive).unwrap();
    let app = fixture.prepare().unwrap();
    assert_eq!(fs::read(&fixture.archive).unwrap(), archive);
    assert_eq!(
      fs::read_link(app.join("Contents/Current")).unwrap(),
      Path::new("MacOS/ctmux-app")
    );
    assert_eq!(fs::read_dir(app.parent().unwrap()).unwrap().count(), 1);
  }

  #[tokio::test]
  async fn ad_hoc_signing_seals_a_copy_without_a_provisioning_profile() {
    use crate::updater::sign::{SigningMode, sign_for_install};
    use ctl_client::remote_bundle::desktop::DesktopTarget;

    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let fixture = Fixture::new(|app| {
      let contents = app.join("Contents");
      let source = app.join("fixture.c");
      fs::write(&source, "int main(void) { return 0; }\n").unwrap();
      let executable = contents.join("MacOS/ctmux-app");
      fs::remove_file(&executable).unwrap();
      let result = Command::new("/usr/bin/clang")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
      assert!(result.status.success(), "{result:?}");
      fs::remove_file(source).unwrap();
      for relative in [
        "MacOS/ctmuxd",
        "MacOS/ctl-taskd",
        "Helpers/ctld.app/Contents/MacOS/ctld",
      ] {
        fs::copy(&executable, contents.join(relative)).unwrap();
      }
    });
    let original = fs::read(&fixture.archive).unwrap();
    let target = DesktopTarget::native().unwrap();
    let archive = sign_for_install(
      &fixture.root,
      &fixture.archive,
      VERSION,
      target,
      &fixture.root,
      SigningMode::AdHoc,
      false,
    )
    .await
    .unwrap();
    assert_eq!(fs::read(&fixture.archive).unwrap(), original);
    let transported = fixture.root.join("verify");
    private_fs::create_directory(&transported).unwrap();
    let signed = prepare(&archive, &transported, VERSION).unwrap();
    assert!(
      !signed
        .join("Contents/Helpers/ctld.app/Contents/embedded.provisionprofile")
        .exists()
    );
    let arch = match target {
      DesktopTarget::MacArm64 => "arm64",
      DesktopTarget::MacX64 => "x86_64",
      _ => unreachable!(),
    };
    fs::write(signed.join("Contents/MacOS/ctmuxd"), b"corrupted").unwrap();
    assert!(macos::verify_local_signature(&signed, SigningMode::AdHoc, arch).is_err());
  }

  #[test]
  fn rejects_aliases_for_directories_the_signer_writes_to() {
    let fixture = Fixture::new(|app| {
      let helper = app.join("Contents/Helpers/ctld.app");
      fs::rename(&helper, app.join("Contents/HelperCopy")).unwrap();
      symlink("../HelperCopy", helper).unwrap();
    });
    assert!(
      fixture
        .prepare()
        .unwrap_err()
        .to_string()
        .contains("real desktop bundle directory")
    );
  }

  #[test]
  fn rejects_a_different_nested_helper_or_unsigned_input_without_sidecars() {
    let fixture = Fixture::new(|app| {
      let plist = app.join("Contents/Helpers/ctld.app/Contents/Info.plist");
      fs::write(
        &plist,
        fs::read_to_string(&plist)
          .unwrap()
          .replace("dev.tokn-ai.ctl.ctld", "dev.example.other"),
      )
      .unwrap();
    });
    assert!(
      fixture
        .prepare()
        .unwrap_err()
        .to_string()
        .contains("helper identity")
    );
    let fixture = Fixture::new(|app| {
      fs::remove_file(app.join("Contents/MacOS/ctl-taskd")).unwrap();
    });
    assert_eq!(
      fixture.prepare().unwrap_err().kind(),
      io::ErrorKind::NotFound
    );
  }
}
