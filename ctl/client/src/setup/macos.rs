use super::{
  Error, SetupEvent, SetupOutcome,
  install::Session,
  manifest::{BUNDLE_IDENTIFIER, MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES, Manifest},
};
use base64::Engine as _;
use ctl_core::executable::PreparedExecutable;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

const RELEASE_ROOT: &str = "https://github.com/tokn-ai/ctl/releases/download";
const MAX_COMMAND_OUTPUT: u64 = 256 * 1024;

pub(super) async fn install(
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, Error> {
  let version = env!("CARGO_PKG_VERSION");
  let target = release_target()?;
  let client = reqwest::Client::builder()
    .https_only(true)
    .connect_timeout(Duration::from_secs(15))
    .read_timeout(Duration::from_secs(30))
    .redirect(reqwest::redirect::Policy::custom(|attempt| {
      if attempt.previous().len() >= 5 || !trusted_url(attempt.url()) {
        attempt.error("untrusted release redirect")
      } else {
        attempt.follow()
      }
    }))
    .user_agent(concat!("ctl/", env!("CARGO_PKG_VERSION")))
    .build()
    .map_err(|error| Error::Download(error.without_url().to_string()))?;
  on_progress(SetupEvent::Manifest);
  let bytes = download(
    &client,
    &format!("{RELEASE_ROOT}/v{version}/ctld-{target}.json"),
    MAX_MANIFEST_BYTES as u64,
    None,
    &on_progress,
  )
  .await?;
  let manifest = Manifest::parse(&bytes, version, target)?;
  let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
  let session = Session::begin(&home, manifest)?;
  let session = if session.reused {
    session
  } else {
    let archive = download(
      &client,
      &format!("{RELEASE_ROOT}/v{version}/{}", session.manifest.archive),
      MAX_ARCHIVE_BYTES,
      Some(session.manifest.archive_size),
      &on_progress,
    )
    .await?;
    unpack(session, archive, &on_progress).await?
  };
  verify_and_activate(session, &on_progress).await
}

pub(super) async fn install_bundled(
  manifest: &[u8],
  archive: &'static [u8],
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, Error> {
  on_progress(SetupEvent::Manifest);
  let manifest = Manifest::parse(manifest, env!("CARGO_PKG_VERSION"), release_target()?)?;
  let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
  install_archive(&home, manifest, archive, &on_progress).await
}

pub(super) async fn install_bundled_development(
  manifest: &[u8],
  archive: &'static [u8],
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, Error> {
  on_progress(SetupEvent::Manifest);
  let manifest =
    Manifest::parse_development(manifest, env!("CARGO_PKG_VERSION"), release_target()?)?;
  let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
  install_archive(&home, manifest, archive, &on_progress).await
}

fn release_target() -> Result<&'static str, Error> {
  match std::env::consts::ARCH {
    "aarch64" => Ok("aarch64-apple-darwin"),
    "x86_64" => Ok("x86_64-apple-darwin"),
    _ => Err(Error::UnsupportedPlatform),
  }
}

async fn install_archive(
  home: &Path,
  manifest: Manifest,
  archive: impl AsRef<[u8]> + Send + 'static,
  on_progress: &impl Fn(SetupEvent),
) -> Result<SetupOutcome, Error> {
  let session = Session::begin(home, manifest)?;
  let session = if session.reused {
    session
  } else {
    unpack(session, archive, on_progress).await?
  };
  verify_and_activate(session, on_progress).await
}

async fn unpack(
  session: Session,
  archive: impl AsRef<[u8]> + Send + 'static,
  on_progress: &impl Fn(SetupEvent),
) -> Result<Session, Error> {
  on_progress(SetupEvent::Extracting);
  // The session and cleanup guard stay with the worker when the awaiting
  // future is cancelled. It can finish extraction but cannot select a version.
  tokio::task::spawn_blocking(move || session.unpack(archive.as_ref()))
    .await
    .map_err(io::Error::other)?
}

async fn verify_and_activate(
  session: Session,
  on_progress: &impl Fn(SetupEvent),
) -> Result<SetupOutcome, Error> {
  on_progress(SetupEvent::Verifying);
  let executable = session.executable()?;
  verify(&session).await?;
  // Execute metadata queries only after the selected Apple signature policy.
  let prepared = PreparedExecutable::prepare(
    executable,
    &[
      ("ctld", ctl_ipc::PROTOCOL_VERSION),
      ("ctld_lifecycle", ctl_ipc::lifecycle::PROTOCOL_VERSION),
    ],
  )
  .await?;
  verify_build_identity(&session.manifest, &prepared.info.build)?;
  prepared.verify().await?;
  on_progress(SetupEvent::Activating);
  session.activate()
}

fn verify_build_identity(
  manifest: &Manifest,
  build: &ctl_core::component::ComponentBuildInfo,
) -> Result<(), Error> {
  let source_matches = manifest
    .development
    .as_ref()
    .map_or(!build.dirty, |development| {
      build.source_fingerprint == development.source_fingerprint && build.dirty == development.dirty
    });
  if build.version != manifest.app_version
    || build.source_revision.as_deref() != Some(&manifest.git_revision)
    || !source_matches
  {
    return Err(Error::Verification(
      "helper build identity does not match its release".into(),
    ));
  }
  Ok(())
}

fn trusted_url(url: &reqwest::Url) -> bool {
  url.scheme() == "https"
    && url.port_or_known_default() == Some(443)
    && url.username().is_empty()
    && url.password().is_none()
    && matches!(
      url.host_str(),
      Some(
        "github.com"
          | "release-assets.githubusercontent.com"
          | "objects.githubusercontent.com"
          | "github-releases.githubusercontent.com"
      )
    )
}

async fn download(
  client: &reqwest::Client,
  url: &str,
  limit: u64,
  expected: Option<u64>,
  on_progress: &impl Fn(SetupEvent),
) -> Result<Vec<u8>, Error> {
  let mut response = client
    .get(url)
    .send()
    .await
    .map_err(|error| Error::Download(error.without_url().to_string()))?;
  if response.status() == reqwest::StatusCode::NOT_FOUND {
    return Err(Error::Download(format!(
      "no signed ctld artifact is published for ctl {}; publish the matching signed release before running setup",
      env!("CARGO_PKG_VERSION")
    )));
  }
  response = response
    .error_for_status()
    .map_err(|error| Error::Download(error.without_url().to_string()))?;
  if response
    .content_length()
    .is_some_and(|length| length > limit || expected.is_some_and(|expected| length != expected))
  {
    return Err(Error::InvalidRelease("unexpected download size".into()));
  }
  let mut bytes = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|error| Error::Download(error.without_url().to_string()))?
  {
    if bytes.len() as u64 + chunk.len() as u64 > limit {
      return Err(Error::InvalidRelease(
        "download exceeds its size limit".into(),
      ));
    }
    bytes.extend_from_slice(&chunk);
    if let Some(total_bytes) = expected {
      if bytes.len() as u64 > total_bytes {
        return Err(Error::InvalidRelease(
          "download exceeds declared size".into(),
        ));
      }
      on_progress(SetupEvent::Downloading {
        received_bytes: bytes.len() as u64,
        total_bytes,
      });
    }
  }
  if expected.is_some_and(|expected| bytes.len() as u64 != expected) {
    return Err(Error::InvalidRelease("truncated download".into()));
  }
  Ok(bytes)
}

async fn tool(program: &str, args: &[&std::ffi::OsStr]) -> Result<std::process::Output, Error> {
  tool_with_input(program, args, None).await
}

async fn tool_with_input(
  program: &str,
  args: &[&std::ffi::OsStr],
  input: Option<&[u8]>,
) -> Result<std::process::Output, Error> {
  tokio::time::timeout(Duration::from_secs(120), async {
    let mut child = Command::new(program)
      .args(args)
      .env("LC_ALL", "C")
      .stdin(if input.is_some() {
        Stdio::piped()
      } else {
        Stdio::null()
      })
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .kill_on_drop(true)
      .spawn()?;
    let ((), output, diagnostics, status) = tokio::try_join!(
      tool_input(child.stdin.take(), input),
      tool_output(child.stdout.take().unwrap()),
      tool_output(child.stderr.take().unwrap()),
      child.wait()
    )?;
    Ok(std::process::Output {
      status,
      stdout: output,
      stderr: diagnostics,
    })
  })
  .await
  .map_err(|_| Error::Verification(format!("{program} timed out")))?
  .map_err(Error::Io)
}

async fn tool_input(
  stdin: Option<tokio::process::ChildStdin>,
  input: Option<&[u8]>,
) -> io::Result<()> {
  if let (Some(mut stdin), Some(input)) = (stdin, input) {
    stdin.write_all(input).await?;
  }
  Ok(())
}

async fn tool_output(stream: impl tokio::io::AsyncRead + Unpin) -> io::Result<Vec<u8>> {
  let mut output = Vec::new();
  stream
    .take(MAX_COMMAND_OUTPUT + 1)
    .read_to_end(&mut output)
    .await?;
  if output.len() as u64 > MAX_COMMAND_OUTPUT {
    return Err(io::Error::other(
      "verification tool returned oversized output",
    ));
  }
  Ok(output)
}

fn require_success(output: &std::process::Output, stage: &str) -> Result<(), Error> {
  if !output.status.success() {
    let diagnostic: String = String::from_utf8_lossy(if output.stderr.is_empty() {
      &output.stdout
    } else {
      &output.stderr
    })
    .chars()
    .filter(|character| !character.is_control() || character.is_whitespace())
    .take(1024)
    .collect();
    return Err(Error::Verification(format!("{stage}: {diagnostic}")));
  }
  Ok(())
}

async fn plist_json(path: &Path, key: &str) -> Result<serde_json::Value, Error> {
  let output = tool(
    "/usr/bin/plutil",
    &[
      "-extract".as_ref(),
      key.as_ref(),
      "xml1".as_ref(),
      "-o".as_ref(),
      "-".as_ref(),
      path.as_os_str(),
    ],
  )
  .await?;
  require_success(&output, "read signed bundle metadata")?;
  // plutil validates the entire input against the destination format, even for
  // -extract. Isolate the XML first: full profiles contain non-JSON Date/Data.
  let isolated = tool_with_input(
    "/usr/bin/plutil",
    &[
      "-convert".as_ref(),
      "json".as_ref(),
      "-o".as_ref(),
      "-".as_ref(),
      "--".as_ref(),
      "-".as_ref(),
    ],
    Some(&output.stdout),
  )
  .await?;
  require_success(&isolated, "read isolated bundle metadata")?;
  serde_json::from_slice(&isolated.stdout).map_err(|error| Error::Verification(error.to_string()))
}

async fn plist_string(path: &Path, key: &str) -> Result<String, Error> {
  let output = tool(
    "/usr/bin/plutil",
    &[
      "-extract".as_ref(),
      key.as_ref(),
      "raw".as_ref(),
      "-o".as_ref(),
      "-".as_ref(),
      path.as_os_str(),
    ],
  )
  .await?;
  require_success(&output, "read signed bundle identity")?;
  String::from_utf8(output.stdout)
    .map(|value| value.trim().to_owned())
    .map_err(|error| Error::Verification(error.to_string()))
}

async fn verify(session: &Session) -> Result<(), Error> {
  verify_signature(session).await?;
  let profile = profile_entitlements(session).await?;
  let signed = signed_entitlements(session).await?;
  let team = &session.manifest.team_identifier;
  if session.manifest.development.is_some() {
    validate_development_entitlements(&signed, &profile, team)
  } else {
    validate_entitlements(&signed, team)?;
    verify_notarization(&session.app()).await
  }
}

async fn verify_signature(session: &Session) -> Result<(), Error> {
  let app = session.app();
  let team = &session.manifest.team_identifier;
  // The publisher Team ID comes from the fixed repository's HTTPS manifest or
  // the manifest embedded alongside the signed helper in this CLI build.
  // The requirement additionally checks Apple's Developer ID certificate chain.
  let development = session.manifest.development.is_some();
  let requirement = if development {
    format!(
      "=anchor apple generic and identifier \"{BUNDLE_IDENTIFIER}\" and certificate leaf[subject.OU] = \"{team}\""
    )
  } else {
    developer_id_requirement(team)
  };
  let output = tool(
    "/usr/bin/codesign",
    &[
      "--verify".as_ref(),
      "--strict".as_ref(),
      "--all-architectures".as_ref(),
      "-R".as_ref(),
      requirement.as_ref(),
      app.as_os_str(),
    ],
  )
  .await?;
  require_success(&output, "Apple code signature")?;
  let info = app.join("Contents/Info.plist");
  if plist_string(&info, "CFBundleIdentifier").await? != BUNDLE_IDENTIFIER
    || plist_string(&info, "CFBundleShortVersionString").await? != session.manifest.app_version
    || plist_string(&info, "CFBundleExecutable").await? != "ctld"
  {
    return Err(Error::Verification(
      "bundle identity does not match the release".into(),
    ));
  }
  Ok(())
}

async fn profile_entitlements(session: &Session) -> Result<serde_json::Value, Error> {
  let app = session.app();
  let team = &session.manifest.team_identifier;
  let development = session.manifest.development.is_some();
  let profile = session.work().join("profile.plist");
  let output = tool(
    "/usr/bin/security",
    &[
      "cms".as_ref(),
      "-D".as_ref(),
      "-i".as_ref(),
      app.join("Contents/embedded.provisionprofile").as_os_str(),
      "-o".as_ref(),
      profile.as_os_str(),
    ],
  )
  .await?;
  require_success(&output, "distribution provisioning profile")?;
  if !development && plist_string(&profile, "ProvisionsAllDevices").await? != "true" {
    return Err(Error::Verification(
      "helper does not have a Developer ID distribution profile".into(),
    ));
  }
  let entitlements = plist_json(&profile, "Entitlements").await?;
  if development {
    validate_entitlement_identity(&entitlements, team)?;
    verify_development_profile(session, &profile).await?;
  } else {
    validate_entitlements(&entitlements, team)?;
  }
  Ok(entitlements)
}

async fn signed_entitlements(session: &Session) -> Result<serde_json::Value, Error> {
  let app = session.app();
  let signed = tool(
    "/usr/bin/codesign",
    &[
      "-d".as_ref(),
      "--entitlements".as_ref(),
      ":-".as_ref(),
      app.as_os_str(),
    ],
  )
  .await?;
  require_success(&signed, "signed entitlements")?;
  let signed_path = session.work().join("signed-entitlements.plist");
  std::fs::write(&signed_path, signed.stdout)?;
  let signed = tool(
    "/usr/bin/plutil",
    &[
      "-convert".as_ref(),
      "json".as_ref(),
      "-o".as_ref(),
      "-".as_ref(),
      signed_path.as_os_str(),
    ],
  )
  .await?;
  require_success(&signed, "signed entitlements")?;
  let signed: serde_json::Value = serde_json::from_slice(&signed.stdout)
    .map_err(|error| Error::Verification(error.to_string()))?;
  Ok(signed)
}

async fn verify_notarization(app: &Path) -> Result<(), Error> {
  let assessment = tool(
    "/usr/sbin/spctl",
    &[
      "--assess".as_ref(),
      "--type".as_ref(),
      "execute".as_ref(),
      "--verbose=2".as_ref(),
      app.as_os_str(),
    ],
  )
  .await?;
  require_success(&assessment, "notarization assessment")?;
  if !String::from_utf8_lossy(&assessment.stderr).contains("source=Notarized Developer ID") {
    return Err(Error::Verification(
      "Gatekeeper did not confirm a notarized Developer ID helper".into(),
    ));
  }
  Ok(())
}

fn validate_entitlements(entitlements: &serde_json::Value, team: &str) -> Result<(), Error> {
  validate_entitlement_identity(entitlements, team)?;
  if ["get-task-allow", "com.apple.security.get-task-allow"]
    .iter()
    .any(|key| entitlements.get(key).is_some_and(|value| value != false))
  {
    return Err(Error::Verification(
      "profile and signed entitlements must authorize the release identity without debugging"
        .into(),
    ));
  }
  Ok(())
}

fn validate_entitlement_identity(
  entitlements: &serde_json::Value,
  team: &str,
) -> Result<(), Error> {
  if entitlements["com.apple.application-identifier"] != format!("{team}.{BUNDLE_IDENTIFIER}")
    || entitlements["com.apple.developer.team-identifier"] != team
  {
    return Err(Error::Verification(
      "profile and signed entitlements must authorize the helper's application and team".into(),
    ));
  }
  Ok(())
}

fn validate_development_entitlements(
  signed: &serde_json::Value,
  profile: &serde_json::Value,
  team: &str,
) -> Result<(), Error> {
  validate_entitlement_identity(signed, team)?;
  for key in ["get-task-allow", "com.apple.security.get-task-allow"] {
    if signed.get(key).is_some_and(|value| !value.is_boolean())
      || (signed.get(key).and_then(serde_json::Value::as_bool) == Some(true)
        && profile.get(key).and_then(serde_json::Value::as_bool) != Some(true))
    {
      return Err(Error::Verification(
        "development debugging entitlement is not authorized by its profile".into(),
      ));
    }
  }
  Ok(())
}

async fn verify_development_profile(session: &Session, profile: &Path) -> Result<(), Error> {
  let team = &session.manifest.team_identifier;
  if plist_json(profile, "TeamIdentifier").await? != serde_json::json!([team]) {
    return Err(Error::Verification(
      "development profile belongs to a different team".into(),
    ));
  }
  let expiration = plist_string(profile, "ExpirationDate").await?;
  verify_profile_expiration(&expiration).await?;
  let certificates = tool(
    "/usr/bin/plutil",
    &[
      "-extract".as_ref(),
      "DeveloperCertificates".as_ref(),
      "xml1".as_ref(),
      "-o".as_ref(),
      "-".as_ref(),
      profile.as_os_str(),
    ],
  )
  .await?;
  require_success(&certificates, "read profile signing certificates")?;
  let certificates = decode_profile_certificates(&certificates.stdout)?;
  let prefix = session.work().join("signing-certificate-");
  extract_signing_certificates(&session.app(), &prefix).await?;
  let leaf = std::fs::File::from(
    rustix::fs::open(
      session.work().join("signing-certificate-0"),
      rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC,
      rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)?,
  );
  let metadata = leaf.metadata()?;
  if !metadata.is_file()
    || metadata.uid() != rustix::process::getuid().as_raw()
    || metadata.mode() & 0o022 != 0
  {
    return Err(Error::Verification(
      "signing certificate is not a regular file".into(),
    ));
  }
  let mut bytes = Vec::new();
  leaf.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
  if bytes.is_empty() || bytes.len() > 64 * 1024 || !certificates.contains(&bytes) {
    return Err(Error::Verification(
      "helper signing certificate is not authorized by its development profile".into(),
    ));
  }
  Ok(())
}

async fn extract_signing_certificates(app: &Path, prefix: &Path) -> Result<(), Error> {
  // codesign's optional long-option arguments require the equals form. A
  // separate prefix is instead parsed as another code object to inspect.
  let mut argument = std::ffi::OsString::from("--extract-certificates=");
  argument.push(prefix);
  let extracted = tool(
    "/usr/bin/codesign",
    &["-d".as_ref(), argument.as_os_str(), app.as_os_str()],
  )
  .await?;
  require_success(&extracted, "extract helper signing certificate")
}

fn decode_profile_certificates(xml: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
  let xml = std::str::from_utf8(xml).map_err(|error| Error::Verification(error.to_string()))?;
  let mut certificates = Vec::new();
  for data in xml.split("<data>").skip(1) {
    let data = data
      .split_once("</data>")
      .ok_or_else(|| Error::Verification("invalid certificate plist data".into()))?
      .0;
    let encoded: String = data
      .chars()
      .filter(|character| !character.is_ascii_whitespace())
      .collect();
    let certificate = base64::engine::general_purpose::STANDARD
      .decode(encoded)
      .map_err(|error| Error::Verification(error.to_string()))?;
    if certificate.is_empty() || certificate.len() > 64 * 1024 || certificates.len() >= 32 {
      return Err(Error::Verification(
        "invalid profile certificate size or count".into(),
      ));
    }
    certificates.push(certificate);
  }
  if certificates.is_empty() {
    return Err(Error::Verification(
      "development profile contains no signing certificates".into(),
    ));
  }
  Ok(certificates)
}

async fn verify_profile_expiration(value: &str) -> Result<(), Error> {
  if value.len() != 20
    || value.bytes().enumerate().any(|(index, byte)| match index {
      4 | 7 => byte != b'-',
      10 => byte != b'T',
      13 | 16 => byte != b':',
      19 => byte != b'Z',
      _ => !byte.is_ascii_digit(),
    })
  {
    return Err(Error::Verification(
      "invalid development profile expiration date".into(),
    ));
  }
  let parsed = tool(
    "/bin/date",
    &[
      "-j".as_ref(),
      "-u".as_ref(),
      "-f".as_ref(),
      "%Y-%m-%dT%H:%M:%SZ".as_ref(),
      value.as_ref(),
      "+%s".as_ref(),
    ],
  )
  .await?;
  require_success(&parsed, "parse development profile expiration")?;
  let expires: u64 = String::from_utf8_lossy(&parsed.stdout)
    .trim()
    .parse()
    .map_err(|error: std::num::ParseIntError| Error::Verification(error.to_string()))?;
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_err(io::Error::other)?
    .as_secs();
  if expires <= now {
    return Err(Error::Verification(
      "development provisioning profile has expired".into(),
    ));
  }
  Ok(())
}

fn developer_id_requirement(team: &str) -> String {
  // Apple's requirement arguments interpret a plain string as a file path.
  format!(
    "=anchor apple generic and identifier \"{BUNDLE_IDENTIFIER}\" and certificate leaf[subject.OU] = \"{team}\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
  )
}

#[cfg(test)]
mod tests {
  use super::super::tests::{Home, compressed, contents, development_bundle, release};
  use super::*;
  use std::sync::Mutex;

  fn assert_staging_clean(home: &Path) {
    let root = ctl_ipc::managed::component_directory(home);
    assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
      !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".setup-")
    }));
  }

  #[test]
  fn development_identity_allows_dirty_sources_only_when_the_embedded_metadata_matches() {
    let (_, development) = development_bundle();
    let identity = development.development.as_ref().unwrap();
    let build = ctl_core::component::ComponentBuildInfo {
      version: development.app_version.clone(),
      source_revision: Some(development.git_revision.clone()),
      source_fingerprint: identity.source_fingerprint.clone(),
      dirty: identity.dirty,
    };
    verify_build_identity(&development, &build).unwrap();
    for field in 0..4 {
      let mut changed = build.clone();
      match field {
        0 => changed.source_fingerprint = "a".repeat(64),
        1 => changed.dirty = !changed.dirty,
        2 => changed.source_revision = Some("f".repeat(40)),
        _ => changed.version = "0.2.0".into(),
      }
      assert!(verify_build_identity(&development, &changed).is_err());
    }
    assert!(verify_build_identity(&super::super::manifest::fixture(), &build).is_err());
  }

  #[tokio::test]
  async fn development_unsigned_bundle_is_rejected_without_a_staple_or_production_selection() {
    let home = Home::new();
    let (bytes, manifest) = development_bundle();
    let result = install_archive(&home.0, manifest, bytes, &|_| {}).await;
    assert!(matches!(result, Err(Error::Verification(_))));
    assert!(
      ctl_ipc::managed::resolve_executable(&home.0)
        .unwrap()
        .is_none()
    );
    assert_staging_clean(&home.0);
  }

  #[test]
  fn development_debugging_still_requires_profile_authorization() {
    let team = "ABCDEFGHIJ";
    let mut signed = serde_json::json!({"com.apple.application-identifier": format!("{team}.{BUNDLE_IDENTIFIER}"), "com.apple.developer.team-identifier": team});
    let mut profile = signed.clone();
    validate_development_entitlements(&signed, &profile, team).unwrap();
    signed["get-task-allow"] = true.into();
    assert!(validate_development_entitlements(&signed, &profile, team).is_err());
    profile["get-task-allow"] = true.into();
    validate_development_entitlements(&signed, &profile, team).unwrap();
    assert!(validate_entitlements(&signed, team).is_err());
    signed["get-task-allow"] = "true".into();
    assert!(validate_development_entitlements(&signed, &profile, team).is_err());
  }

  #[tokio::test]
  async fn development_profile_expiration_and_certificate_data_are_validated() {
    verify_profile_expiration("2099-01-01T00:00:00Z")
      .await
      .unwrap();
    assert!(
      verify_profile_expiration("2000-01-01T00:00:00Z")
        .await
        .is_err()
    );
    assert!(verify_profile_expiration("not-a-date").await.is_err());
    assert_eq!(
      decode_profile_certificates(b"<plist><array><data> AQI=\n </data></array></plist>").unwrap(),
      vec![vec![1, 2]]
    );
    for invalid in [
      b"<array/>".as_slice(),
      b"<array><data>bad</data></array>",
      b"<array><data></data></array>",
    ] {
      assert!(decode_profile_certificates(invalid).is_err());
    }
  }

  #[tokio::test]
  async fn native_codesign_treats_certificate_prefix_as_its_optional_argument() {
    let home = Home::new();
    let prefix = home.0.join("requested certificate prefix-");
    // Display a native signed tool without executing it or using a signing
    // identity. With a separate optional argument, codesign treats this missing
    // prefix as a second code object and fails before inspecting the tool.
    extract_signing_certificates(Path::new("/usr/bin/codesign"), &prefix)
      .await
      .unwrap();
  }

  #[tokio::test]
  #[ignore = "requires a provisioned signed development helper"]
  async fn provisioned_development_helper_installs_and_reuses_without_selecting_production() {
    // Use an explicit temporary installation directory while retaining normal
    // macOS trust-service/Keychain context. Fixture generation/signing happens
    // separately; this test only verifies and queries the existing signed app.
    let directory = std::path::PathBuf::from(
      std::env::var_os("CTL_TEST_BUNDLED_CTLD_DIR")
        .expect("set CTL_TEST_BUNDLED_CTLD_DIR to a signed development payload directory"),
    );
    let target = release_target().unwrap();
    let manifest = Manifest::parse_development(
      &std::fs::read(directory.join(format!("ctld-{target}.json"))).unwrap(),
      env!("CARGO_PKG_VERSION"),
      target,
    )
    .unwrap();
    let archive = std::fs::read(directory.join(&manifest.archive)).unwrap();
    let home = Home::new();
    let installed = install_archive(&home.0, manifest.clone(), archive.clone(), &|_| {})
      .await
      .unwrap();
    assert!(!installed.reused);
    assert!(installed.executable.is_file());
    assert_staging_clean(&home.0);
    let reused = install_archive(&home.0, manifest, archive, &|_| {})
      .await
      .unwrap();
    assert!(reused.reused);
    assert_eq!(reused.executable, installed.executable);
    assert_staging_clean(&home.0);
    assert!(
      ctl_ipc::managed::resolve_executable(&home.0)
        .unwrap()
        .is_none()
    );
    assert_eq!(
      std::fs::symlink_metadata(ctl_ipc::managed::component_directory(&home.0).join("current"))
        .unwrap_err()
        .kind(),
      io::ErrorKind::NotFound
    );
  }

  #[tokio::test]
  async fn bundled_helper_requires_the_cli_version_and_target() {
    let mut version = super::super::manifest::fixture();
    version.app_version = "0.2.0".into();
    let mut target = super::super::manifest::fixture();
    target.target = if release_target().unwrap() == "aarch64-apple-darwin" {
      "x86_64-apple-darwin"
    } else {
      "aarch64-apple-darwin"
    }
    .into();
    for manifest in [version, target] {
      let bytes = serde_json::to_vec(&manifest).unwrap();
      let result = install_bundled(&bytes, b"unused archive", |_| {}).await;
      assert!(matches!(result, Err(Error::InvalidRelease(_))));
    }
  }

  #[tokio::test]
  async fn bundled_archive_checksum_failure_never_reaches_signature_checks_or_activation() {
    let home = Home::new();
    let bytes = compressed(&contents(None));
    let manifest = release(&bytes);
    let mut corrupt = bytes;
    corrupt[0] ^= 1;
    let events = Mutex::new(Vec::new());
    let result = install_archive(&home.0, manifest, corrupt, &|event| {
      events.lock().unwrap().push(event);
    })
    .await;
    assert!(matches!(result, Err(Error::InvalidRelease(_))));
    assert!(matches!(
      &events.into_inner().unwrap()[..],
      [SetupEvent::Extracting]
    ));
    assert!(
      ctl_ipc::managed::resolve_executable(&home.0)
        .unwrap()
        .is_none()
    );
    assert_staging_clean(&home.0);
  }

  #[tokio::test]
  async fn bundled_reuse_still_verifies_apple_identity_before_executing_the_helper() {
    let home = Home::new();
    let bytes = compressed(&contents(None));
    let manifest = release(&bytes);
    let installed = Session::begin(&home.0, manifest.clone())
      .unwrap()
      .unpack(&bytes)
      .unwrap()
      .activate()
      .unwrap();
    let marker = home.0.join("unexpected-helper-execution");
    std::fs::write(
      &installed.executable,
      format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    let events = Mutex::new(Vec::new());
    let result = install_archive(&home.0, manifest, bytes, &|event| {
      events.lock().unwrap().push(event);
    })
    .await;
    assert!(matches!(result, Err(Error::Verification(_))));
    assert!(matches!(
      &events.into_inner().unwrap()[..],
      [SetupEvent::Verifying]
    ));
    assert!(!marker.exists());
    assert_eq!(
      ctl_ipc::managed::resolve_executable(&home.0)
        .unwrap()
        .unwrap(),
      installed.executable
    );
    assert_staging_clean(&home.0);
  }

  struct File(std::path::PathBuf);
  impl File {
    fn new() -> Self {
      Self(std::env::temp_dir().join(format!("ctld-apple-test-{}", uuid::Uuid::new_v4())))
    }
  }
  impl Drop for File {
    fn drop(&mut self) {
      let _ = std::fs::remove_file(&self.0);
    }
  }

  #[tokio::test]
  async fn apple_requirement_compiler_accepts_literal_developer_id_policy() {
    let output = File::new();
    let requirement = developer_id_requirement("ABCDEFGHIJ");
    assert!(requirement.starts_with("=anchor "));
    let compiled = tool(
      "/usr/bin/csreq",
      &[
        "-r".as_ref(),
        requirement.as_ref(),
        "-b".as_ref(),
        output.0.as_os_str(),
      ],
    )
    .await
    .unwrap();
    require_success(&compiled, "compile Developer ID requirement").unwrap();
    assert_ne!(std::fs::read(&output.0).unwrap(), Vec::<u8>::new());
  }

  #[tokio::test]
  async fn native_plutil_extracts_authorization_without_converting_profile_dates_or_data() {
    let profile = File::new();
    std::fs::write(
      &profile.0,
      br#"<?xml version="1.0" encoding="UTF-8"?>
      <plist version="1.0"><dict>
      <key>Entitlements</key><dict>
        <key>com.apple.application-identifier</key><string>ABCDEFGHIJ.dev.tokn-ai.ctl.ctld</string>
        <key>com.apple.developer.team-identifier</key><string>ABCDEFGHIJ</string>
        <key>get-task-allow</key><false/>
      </dict>
      <key>ProvisionsAllDevices</key><true/>
      <key>ExpirationDate</key><date>2099-01-01T00:00:00Z</date>
      <key>DeveloperCertificates</key><array><data>AQI=</data></array>
      </dict></plist>"#,
    )
    .unwrap();
    let entitlements = plist_json(&profile.0, "Entitlements").await.unwrap();
    validate_entitlements(&entitlements, "ABCDEFGHIJ").unwrap();
    assert_eq!(
      plist_string(&profile.0, "ProvisionsAllDevices")
        .await
        .unwrap(),
      "true"
    );
  }

  #[tokio::test]
  async fn excessive_tool_output_terminates_the_process_without_waiting_for_its_timeout() {
    let result = tokio::time::timeout(Duration::from_secs(3), tool("/usr/bin/yes", &[]))
      .await
      .unwrap();
    assert!(
      matches!(result, Err(Error::Io(error)) if error.to_string().contains("oversized output"))
    );
  }
  #[test]
  fn releases_never_redirect_to_http_or_unrelated_hosts() {
    for url in [
      "https://github.com/tokn-ai/ctl",
      "https://release-assets.githubusercontent.com/asset",
    ] {
      assert!(trusted_url(&reqwest::Url::parse(url).unwrap()));
    }
    for url in [
      "http://github.com/asset",
      "https://github.com.example/asset",
      "https://github.com:8443/asset",
      "https://user:password@github.com/asset",
    ] {
      assert!(!trusted_url(&reqwest::Url::parse(url).unwrap()));
    }
  }

  #[test]
  fn distribution_identity_rejects_other_teams_and_debugging() {
    let team = "ABCDEFGHIJ";
    let mut entitlements = serde_json::json!({"com.apple.application-identifier": format!("{team}.{BUNDLE_IDENTIFIER}"), "com.apple.developer.team-identifier": team});
    assert!(validate_entitlements(&entitlements, team).is_ok());
    entitlements["com.apple.security.get-task-allow"] = true.into();
    assert!(validate_entitlements(&entitlements, team).is_err());
    entitlements["com.apple.security.get-task-allow"] = false.into();
    assert!(validate_entitlements(&entitlements, "KLMNOPQRST").is_err());
  }
}
