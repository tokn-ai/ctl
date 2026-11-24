use std::{io, path::Path};
use tokio::{fs, process::Command};

use super::Runner;

pub(super) async fn sign(
  app: &Path,
  stage: &Path,
  version: &str,
  profile: &Path,
  runner: &Runner,
) -> io::Result<String> {
  let packaging = stage.join("ctld-packaging");
  fs::create_dir(&packaging).await?;
  for (name, contents) in [
    (
      "package-ctld-app.sh",
      include_bytes!("ctld/package-ctld-app.sh").as_slice(),
    ),
    ("Info.plist", include_bytes!("ctld/Info.plist").as_slice()),
    (
      "Entitlements.plist",
      include_bytes!("ctld/Entitlements.plist").as_slice(),
    ),
  ] {
    fs::write(packaging.join(name), contents).await?;
  }

  let identity = packaging.join("signing-identity");
  let helper = app.join("Contents/Helpers/ctld.app");
  runner
    .output(
      Command::new("/bin/sh")
        .arg(packaging.join("package-ctld-app.sh"))
        .arg(helper.join("Contents/MacOS/ctld"))
        .arg(helper)
        .arg(version)
        .env("CTLD_PROVISIONING_PROFILE", profile)
        .env("CTLD_REQUIRE_DISTRIBUTION_SIGNING", "true")
        .env("CTLD_SIGNING_TIMESTAMP", "secure")
        .env("CTLD_SIGNING_IDENTITY_OUTPUT", &identity)
        .env("TMPDIR", &packaging),
    )
    .await?;
  let fingerprint = fs::read_to_string(identity).await?;
  let fingerprint = fingerprint.trim();
  if fingerprint.len() != 40 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "ctld signing did not return a Developer ID certificate fingerprint",
    ));
  }
  Ok(fingerprint.to_owned())
}
