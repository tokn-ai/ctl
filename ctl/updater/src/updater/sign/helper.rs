use std::{io, path::Path};
use tokio::{fs, process::Command};

use super::Runner;

pub(super) enum Identity<'a> {
  DeveloperId(&'a Path),
  Development(&'a Path),
  AdHoc,
}

pub(super) async fn sign(
  binary: &Path,
  app: &Path,
  stage: &Path,
  version: &str,
  identity: Identity<'_>,
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

  let (profile, distribution, timestamp) = match identity {
    Identity::DeveloperId(profile) => (Some(profile), "true", "secure"),
    Identity::Development(profile) => (Some(profile), "false", "none"),
    Identity::AdHoc => (None, "false", "none"),
  };
  let identity_file = packaging.join("signing-identity");
  let mut package = Command::new("/bin/sh");
  package
    .arg(packaging.join("package-ctld-app.sh"))
    .arg(binary)
    .arg(app)
    .arg(version)
    .env_remove("CTLD_PROVISIONING_PROFILE")
    .env("CTLD_REQUIRE_DISTRIBUTION_SIGNING", distribution)
    .env("CTLD_SIGNING_TIMESTAMP", timestamp)
    .env("CTLD_SIGNING_IDENTITY_OUTPUT", &identity_file)
    .env("TMPDIR", &packaging);
  if let Some(profile) = profile {
    package.env("CTLD_PROVISIONING_PROFILE", profile);
  }
  runner.output(&mut package).await?;
  if matches!(identity, Identity::AdHoc) {
    runner
      .output(
        Command::new("/usr/bin/codesign")
          .args(["--force", "--timestamp=none", "--sign", "-"])
          .arg(app),
      )
      .await?;
    return Ok("-".into());
  }
  let fingerprint = fs::read_to_string(identity_file).await?;
  let fingerprint = fingerprint.trim();
  if fingerprint.len() != 40 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "ctld signing did not return a certificate fingerprint",
    ));
  }
  Ok(fingerprint.to_owned())
}
