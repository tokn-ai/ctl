use super::Runner;
use base64::Engine as _;
use ctl_core::{paths, private_fs};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::process::Command;

const BUNDLE_ID: &str = "dev.tokn-ai.ctl.ctld";
const PROJECT: &str = "ctld-provisioning.xcodeproj";
const PROFILE_LIMIT: u64 = 2 * 1024 * 1024;
const PROFILE_OVERRIDE: &str = "CTLD_PROVISIONING_PROFILE";

pub(super) struct Profile {
  pub(super) path: PathBuf,
  pub(super) team: String,
  _stage: Stage,
}

struct Stage(PathBuf);

impl Stage {
  fn new(parent: &Path) -> io::Result<Self> {
    let path = parent.join(format!("provisioning-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path)?;
    Ok(Self(path))
  }
}

impl Drop for Stage {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[derive(Debug)]
struct Identity {
  name: String,
  certificate: Vec<u8>,
}

#[derive(Debug)]
struct Authorization {
  team: String,
  expires: SystemTime,
  certificates: Vec<Vec<u8>>,
}

impl Authorization {
  fn from_plist(bytes: &[u8]) -> io::Result<Self> {
    #[derive(serde::Deserialize)]
    struct Entitlements {
      #[serde(rename = "com.apple.application-identifier")]
      application: String,
      #[serde(rename = "com.apple.developer.team-identifier")]
      team: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Provisioning {
      entitlements: Entitlements,
      expiration_date: plist::Date,
      team_identifier: Vec<String>,
      developer_certificates: Vec<plist::Data>,
      #[serde(default)]
      provisions_all_devices: bool,
    }
    let decoded: Provisioning =
      plist::from_bytes(bytes).map_err(|_| invalid("invalid ctld provisioning profile"))?;
    let team = decoded.entitlements.team;
    if !valid_team(&team)
      || decoded.entitlements.application != format!("{team}.{BUNDLE_ID}")
      || decoded.team_identifier != [team.as_str()]
      || decoded.provisions_all_devices
      || decoded.developer_certificates.is_empty()
      || decoded.developer_certificates.len() > 32
      || decoded
        .developer_certificates
        .iter()
        .any(|certificate| !(1..=64 * 1024).contains(&certificate.as_ref().len()))
    {
      return Err(invalid(
        "expected a development provisioning profile for ctld",
      ));
    }
    Ok(Self {
      team,
      expires: decoded.expiration_date.into(),
      certificates: decoded
        .developer_certificates
        .into_iter()
        .map(Into::into)
        .collect(),
    })
  }

  fn verify(&self, team: &str) -> io::Result<()> {
    if self.team != team {
      return Err(invalid(
        "ctld provisioning profile belongs to a different Apple Team",
      ));
    }
    if self.expires <= SystemTime::now() {
      return Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "ctld provisioning profile has expired",
      ));
    }
    Ok(())
  }

  fn authorizes(&self, identities: &[Identity]) -> bool {
    identities
      .iter()
      .any(|identity| self.certificates.contains(&identity.certificate))
  }
}

pub(in crate::updater) fn verify_embedded_profile(path: &Path, team: &str) -> io::Result<()> {
  validate_profile_file(path)?;
  let output = std::process::Command::new("/usr/bin/security")
    .args(["cms", "-D", "-i"])
    .arg(path)
    .output()?;
  if !output.status.success() {
    return Err(invalid(
      "could not decode the embedded ctld provisioning profile",
    ));
  }
  Authorization::from_plist(&output.stdout)?.verify(team)
}

pub(super) async fn profile(
  runner: &Runner,
  home: &Path,
  staging: &Path,
  expected_team: Option<&str>,
  interactive: bool,
) -> io::Result<Profile> {
  if expected_team.is_some_and(|team| !valid_team(team)) {
    return Err(invalid("invalid installed Apple Team ID"));
  }
  let stage = Stage::new(staging)?;
  let destination = stage.0.join("ctld.provisionprofile");
  let identities = identities(runner, &stage.0).await?;
  if let Some(path) = std::env::var_os(PROFILE_OVERRIDE).filter(|path| !path.is_empty()) {
    let path = std::path::absolute(path)?;
    snapshot(&path, &destination)?;
    let profile = inspect(runner, &destination).await?;
    let team = expected_team.unwrap_or(&profile.team);
    verify_identity(&profile, team, &identities)?;
    return Ok(Profile {
      path: destination,
      team: team.to_owned(),
      _stage: stage,
    });
  }

  let settings = signing_directory(home)?;
  let cached = if expected_team.is_none() {
    read_team(&settings)?
  } else {
    None
  };
  let can_prompt = interactive && io::stdin().is_terminal() && io::stderr().is_terminal();
  let preferred = expected_team.or(cached.as_deref());
  let configured = if preferred.is_none() {
    configured_team(&settings)?
  } else {
    None
  };
  let team = match preferred.or(configured.as_deref()) {
    Some(team) => team.to_owned(),
    None if identities.is_empty() => {
      let project = install_project(&settings)?;
      if can_prompt {
        runner
          .output(
            Command::new("/usr/bin/open")
              .args(["-a", "Xcode"])
              .arg(&project),
          )
          .await?;
      }
      return Err(io::Error::other(format!(
        "No Apple Development signing identity is available. Sign in to Xcode, open the provisioning project at {}, choose a team under Signing & Capabilities, and build it once. Then rerun with --provision.",
        project.display(),
      )));
    }
    None if identities.len() == 1 => identities.keys().next().expect("one Apple Team").clone(),
    None if can_prompt => choose_team(runner, &identities).await?,
    None => {
      return Err(io::Error::other(
        "Multiple Apple Teams are available. Run with --provision from a terminal to choose one, or set CTLD_PROVISIONING_PROFILE.",
      ));
    }
  };

  if let Some(path) = find_profile(runner, home, &team, &identities).await? {
    snapshot(&path, &destination)?;
    verify_identity(&inspect(runner, &destination).await?, &team, &identities)?;
  } else {
    let embedded = refresh_profile(runner, &stage.0, &team).await?;
    let refreshed = stage.0.join("refreshed-identities");
    private_fs::create_directory(&refreshed)?;
    let refreshed_identities = self::identities(runner, &refreshed).await?;
    snapshot(&embedded, &destination)?;
    let authorization = inspect(runner, &destination).await?;
    verify_identity(&authorization, &team, &refreshed_identities)?;
  }
  if expected_team.is_none() && cached.is_none() {
    save_team(&settings, &team)?;
  }
  Ok(Profile {
    path: destination,
    team,
    _stage: stage,
  })
}

fn verify_identity(
  profile: &Authorization,
  team: &str,
  identities: &BTreeMap<String, Vec<Identity>>,
) -> io::Result<()> {
  profile.verify(team)?;
  if identities
    .get(team)
    .is_some_and(|identities| profile.authorizes(identities))
  {
    Ok(())
  } else {
    Err(invalid(
      "no Apple Development identity matches the ctld provisioning profile",
    ))
  }
}

async fn find_profile(
  runner: &Runner,
  home: &Path,
  team: &str,
  identities: &BTreeMap<String, Vec<Identity>>,
) -> io::Result<Option<PathBuf>> {
  let Some(identities) = identities.get(team) else {
    return Ok(None);
  };
  let mut paths =
    vec![home.join("Library/Application Support/ctmux/signing/ctld.provisionprofile")];
  for directory in [
    "Library/Developer/Xcode/UserData/Provisioning Profiles",
    "Library/MobileDevice/Provisioning Profiles",
  ] {
    profile_files(&home.join(directory), &mut paths)?;
  }
  paths.sort_unstable();
  paths.dedup();
  let mut latest = None;
  for path in paths {
    match inspect(runner, &path).await {
      Ok(profile) if profile.verify(team).is_ok() && profile.authorizes(identities) => {
        if latest
          .as_ref()
          .is_none_or(|(expiry, _)| profile.expires > *expiry)
        {
          latest = Some((profile.expires, path));
        }
      }
      Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
      _ => {}
    }
  }
  Ok(latest.map(|(_, path)| path))
}

fn profile_files(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
  let entries = match fs::read_dir(directory) {
    Ok(entries) => entries,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
    Err(error) => return Err(error),
  };
  for entry in entries {
    let entry = entry?;
    match entry.file_type()? {
      ty if ty.is_dir() => profile_files(&entry.path(), files)?,
      ty if ty.is_file() => files.push(entry.path()),
      _ => {}
    }
  }
  Ok(())
}

async fn inspect(runner: &Runner, path: &Path) -> io::Result<Authorization> {
  validate_profile_file(path)?;
  let output = runner
    .output(
      Command::new("/usr/bin/security")
        .args(["cms", "-D", "-i"])
        .arg(path),
    )
    .await?;
  Authorization::from_plist(&output.stdout)
}

fn validate_profile_file(path: &Path) -> io::Result<()> {
  let metadata = fs::symlink_metadata(path)?;
  if metadata.is_file() && (1..=PROFILE_LIMIT).contains(&metadata.len()) {
    Ok(())
  } else {
    Err(invalid(
      "ctld provisioning profile must be a nonempty regular file",
    ))
  }
}

fn snapshot(source: &Path, destination: &Path) -> io::Result<()> {
  let source = private_fs::options().read(true).open(source)?;
  let metadata = source.metadata()?;
  if !metadata.is_file() || !(1..=PROFILE_LIMIT).contains(&metadata.len()) {
    return Err(invalid(
      "ctld provisioning profile must be a nonempty regular file",
    ));
  }
  let mut destination = private_fs::options()
    .create_new(true)
    .write(true)
    .open(destination)?;
  if io::copy(&mut source.take(PROFILE_LIMIT + 1), &mut destination)? > PROFILE_LIMIT {
    return Err(invalid("ctld provisioning profile exceeds the size limit"));
  }
  Ok(())
}

async fn identities(
  runner: &Runner,
  staging: &Path,
) -> io::Result<BTreeMap<String, Vec<Identity>>> {
  let mut identities = BTreeMap::<String, Vec<Identity>>::new();
  let available = runner
    .output(Command::new("/usr/bin/security").args(["find-identity", "-v", "-p", "codesigning"]))
    .await?;
  let available = development_fingerprints(&available.stdout);
  if available.is_empty() {
    return Ok(identities);
  }
  let certificates = runner
    .output(Command::new("/usr/bin/security").args([
      "find-certificate",
      "-a",
      "-c",
      "Apple Development:",
      "-p",
    ]))
    .await?;
  let certificates = String::from_utf8_lossy(&certificates.stdout);
  let certificate = staging.join("development.der");
  for (_, pem) in certificates
    .split("-----END CERTIFICATE-----")
    .filter_map(|entry| entry.split_once("-----BEGIN CERTIFICATE-----"))
  {
    let encoded: String = pem
      .chars()
      .filter(|character| !character.is_ascii_whitespace())
      .collect();
    let der = base64::engine::general_purpose::STANDARD
      .decode(encoded)
      .map_err(io::Error::other)?;
    fs::write(&certificate, &der)?;
    let subject = runner
      .output(
        Command::new("/usr/bin/openssl")
          .args(["x509", "-inform", "DER", "-in"])
          .arg(&certificate)
          .args([
            "-noout",
            "-subject",
            "-nameopt",
            "multiline",
            "-sha1",
            "-fingerprint",
          ]),
      )
      .await?;
    if let Some((team, name)) = certificate_choice(&subject.stdout, &available) {
      identities.entry(team).or_default().push(Identity {
        name,
        certificate: der,
      });
    }
  }
  Ok(identities)
}

fn development_fingerprints(output: &[u8]) -> BTreeSet<String> {
  String::from_utf8_lossy(output)
    .lines()
    .filter_map(|line| {
      let (index, identity) = line.trim_start().split_once(')')?;
      let (fingerprint, name) = identity.trim_start().split_once(' ')?;
      (index.parse::<u32>().is_ok()
        && fingerprint.len() == 40
        && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        && name.trim_start().starts_with("\"Apple Development:"))
      .then(|| fingerprint.to_ascii_uppercase())
    })
    .collect()
}

fn certificate_choice(output: &[u8], available: &BTreeSet<String>) -> Option<(String, String)> {
  let output = String::from_utf8_lossy(output);
  let subject = |field: &str| {
    output
      .lines()
      .filter_map(|line| line.split_once('='))
      .find_map(|(key, value)| key.trim().eq_ignore_ascii_case(field).then(|| value.trim()))
  };
  let fingerprint = subject("sha1 Fingerprint")?
    .replace(':', "")
    .to_ascii_uppercase();
  let team = subject("organizationalUnitName")?;
  let name = subject("commonName")?
    .strip_prefix("Apple Development:")?
    .trim();
  (available.contains(&fingerprint) && valid_team(team) && !name.is_empty()).then(|| {
    (
      team.to_owned(),
      name
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect(),
    )
  })
}

async fn choose_team(
  runner: &Runner,
  identities: &BTreeMap<String, Vec<Identity>>,
) -> io::Result<String> {
  let terminal = private_fs::options().read(true).open("/dev/tty")?;
  eprintln!("Choose the Apple Team to provision ctld for this Mac.");
  for (index, (team, identities)) in identities.iter().enumerate() {
    let names = identities
      .iter()
      .map(|identity| identity.name.as_str())
      .collect::<BTreeSet<_>>();
    let label = names.into_iter().collect::<Vec<_>>().join(", ");
    eprintln!("  {}. {label} ({team})", index + 1);
  }
  eprint!("Team number: ");
  io::stderr().flush()?;
  let input = runner.cancellable(read_choice(terminal)).await?;
  let index = input
    .trim()
    .parse::<usize>()
    .ok()
    .and_then(|choice| choice.checked_sub(1));
  index
    .and_then(|index| identities.keys().nth(index))
    .cloned()
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::InvalidInput,
        "choose one of the listed Apple Team numbers",
      )
    })
}

async fn read_choice(mut terminal: fs::File) -> io::Result<String> {
  let mut input = Vec::new();
  let mut buffer = [0_u8; 32];
  loop {
    let read = match terminal.read(&mut buffer) {
      Ok(read) => read,
      Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
        // /dev/tty is opened with O_NONBLOCK so cancellation never waits on a
        // terminal read or leaves a blocking input thread behind.
        tokio::time::sleep(Duration::from_millis(50)).await;
        continue;
      }
      Err(error) => return Err(error),
    };
    let data = &buffer[..read];
    let end = data.iter().position(|byte| *byte == b'\n');
    input.extend_from_slice(&data[..end.unwrap_or(data.len())]);
    if input.len() > 32 {
      return Err(invalid("invalid Apple Team selection"));
    }
    if end.is_some() || data.is_empty() {
      return String::from_utf8(input).map_err(io::Error::other);
    }
  }
}

fn signing_directory(home: &Path) -> io::Result<PathBuf> {
  let settings = paths::updater_directory_for_account(home).join("signing");
  let anchor = paths::managed_storage_anchor(&settings)?;
  verify_directory(anchor)?;
  let mut directory = anchor.to_path_buf();
  for segment in settings
    .strip_prefix(anchor)
    .map_err(io::Error::other)?
    .components()
  {
    directory.push(segment);
    ensure_directory(&directory)?;
  }
  Ok(settings)
}

fn verify_directory(path: &Path) -> io::Result<()> {
  if private_fs::is_directory(&fs::symlink_metadata(path)?) {
    Ok(())
  } else {
    Err(invalid("unsafe ctl signing directory"))
  }
}

fn ensure_directory(path: &Path) -> io::Result<()> {
  match private_fs::create_directory(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => verify_directory(path),
    Err(error) => Err(error),
  }
}

fn read_team(settings: &Path) -> io::Result<Option<String>> {
  let file = match private_fs::options()
    .read(true)
    .open(settings.join("provisioning-team"))
  {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  if !private_fs::is_private_file(&file.metadata()?) {
    return Err(invalid(
      "the saved Apple Team selection must be a private regular file",
    ));
  }
  let mut team = String::new();
  file.take(13).read_to_string(&mut team)?;
  let team = team.strip_suffix('\n').unwrap_or(&team);
  if !valid_team(team) {
    return Err(invalid("invalid saved Apple Team selection"));
  }
  Ok(Some(team.to_owned()))
}

fn save_team(settings: &Path, team: &str) -> io::Result<()> {
  let temporary = Stage::new(settings)?;
  let source = temporary.0.join("team");
  let mut file = private_fs::options()
    .create_new(true)
    .write(true)
    .open(&source)?;
  writeln!(file, "{team}")?;
  file.sync_all()?;
  match fs::hard_link(source, settings.join("provisioning-team")) {
    Ok(()) => private_fs::sync_directory(settings),
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
      if read_team(settings)?.as_deref() == Some(team) {
        Ok(())
      } else {
        Err(io::Error::other(
          "another ctlup run chose a different Apple Team; rerun signing",
        ))
      }
    }
    Err(error) => Err(error),
  }
}

fn configured_team(settings: &Path) -> io::Result<Option<String>> {
  let project = settings
    .join("provisioning")
    .join(PROJECT)
    .join("project.pbxproj");
  let contents = match fs::read_to_string(project) {
    Ok(contents) => contents,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  Ok(contents.lines().find_map(|line| {
    let (field, value) = line.trim().split_once('=')?;
    let team = value.trim().trim_end_matches(';').trim_matches('"');
    (field.trim() == "DEVELOPMENT_TEAM" && valid_team(team)).then(|| team.to_owned())
  }))
}

fn install_project(settings: &Path) -> io::Result<PathBuf> {
  let directory = settings.join("provisioning");
  match verify_directory(&directory) {
    Ok(()) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      let stage = Stage::new(settings)?;
      let payload = stage.0.join("project");
      private_fs::create_directory(&payload)?;
      write_project(&payload)?;
      if let Err(error) = fs::rename(&payload, &directory) {
        if matches!(
          error.kind(),
          io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
        ) {
          verify_directory(&directory)?;
        } else {
          return Err(error);
        }
      }
    }
    Err(error) => return Err(error),
  }
  Ok(directory.join(PROJECT))
}

fn write_project(directory: &Path) -> io::Result<PathBuf> {
  let project = directory.join(PROJECT);
  let source = directory.join("ctld-provisioning");
  ensure_directory(&project)?;
  ensure_directory(&source)?;
  for (path, bytes) in [
    (
      project.join("project.pbxproj"),
      include_bytes!("ctld/provisioning/ctld-provisioning.xcodeproj/project.pbxproj").as_slice(),
    ),
    (
      source.join("App.swift"),
      include_bytes!("ctld/provisioning/ctld-provisioning/App.swift").as_slice(),
    ),
    (
      source.join("ctld-provisioning.entitlements"),
      include_bytes!("ctld/provisioning/ctld-provisioning/ctld-provisioning.entitlements")
        .as_slice(),
    ),
  ] {
    match private_fs::options()
      .create_new(true)
      .write(true)
      .open(path)
    {
      Ok(mut file) => file.write_all(bytes)?,
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error),
    }
  }
  Ok(project)
}

async fn refresh_profile(runner: &Runner, directory: &Path, team: &str) -> io::Result<PathBuf> {
  let project = write_project(directory)?;
  let products = directory.join("products");
  let options = [
    format!("DEVELOPMENT_TEAM={team}"),
    format!("CONFIGURATION_BUILD_DIR={}", products.display()),
  ];
  runner.output(
    Command::new("/usr/bin/xcodebuild")
      .arg("-project").arg(project)
      .args(["-scheme", "ctld-provisioning", "-configuration", "Debug", "-destination", "platform=macOS"])
      .arg("-derivedDataPath").arg(directory.join("derived"))
      .args(["-allowProvisioningUpdates", "-allowProvisioningDeviceRegistration"])
      .args(options)
      .arg("build"),
  ).await.map_err(|error| {
    if error.kind() == io::ErrorKind::Interrupted {
      error
    } else {
      io::Error::other(format!(
        "Xcode could not provision ctld. Sign in to Xcode with Apple Team {team}, then retry.\n{error}"
      ))
    }
  })?;
  Ok(products.join("ctld-provisioning.app/Contents/embedded.provisionprofile"))
}

fn valid_team(team: &str) -> bool {
  team.len() == 10
    && team
      .bytes()
      .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn offers_only_current_apple_development_identities_with_a_team() {
    let available = development_fingerprints(
      br#"
      1) 0123456789abcdef0123456789abcdef01234567 "Apple Development: Example"
      2) abcdef0123456789abcdef0123456789abcdef01 "Developer ID Application: Example"
      2 valid identities found
      "#,
    );
    assert_eq!(available.len(), 1);
    let subject = b"subject=\n    commonName = Apple Development: Example\n    organizationalUnitName = TEAM123ABC\nSHA1 Fingerprint=01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67\n";
    assert_eq!(
      certificate_choice(subject, &available),
      Some(("TEAM123ABC".into(), "Example".into())),
    );
    assert_eq!(certificate_choice(subject, &BTreeSet::new()), None);
  }

  #[test]
  fn authorizes_only_the_helper_team_and_matching_development_certificates() {
    let profile = br#"<?xml version="1.0" encoding="UTF-8"?>
      <plist version="1.0">
      <dict>
        <key>Entitlements</key>
        <dict>
          <key>com.apple.application-identifier</key>
          <string>TEAM123ABC.dev.tokn-ai.ctl.ctld</string>
          <key>com.apple.developer.team-identifier</key>
          <string>TEAM123ABC</string>
        </dict>
        <key>TeamIdentifier</key><array><string>TEAM123ABC</string></array>
        <key>ExpirationDate</key><date>2099-01-01T00:00:00Z</date>
        <key>DeveloperCertificates</key><array><data>AQID</data></array>
      </dict>
      </plist>"#;
    let authorization = Authorization::from_plist(profile).unwrap();
    let identities = BTreeMap::from([(
      "TEAM123ABC".into(),
      vec![Identity {
        name: "Example".into(),
        certificate: vec![1, 2, 3],
      }],
    )]);
    assert!(verify_identity(&authorization, "TEAM123ABC", &identities).is_ok());
    assert!(verify_identity(&authorization, "TEAM999ABC", &identities).is_err());
    assert!(verify_identity(&authorization, "TEAM123ABC", &BTreeMap::new()).is_err());
    let expired = String::from_utf8_lossy(profile).replace("2099-", "2000-");
    let expired = Authorization::from_plist(expired.as_bytes()).unwrap();
    assert_eq!(
      expired.verify("TEAM123ABC").unwrap_err().kind(),
      io::ErrorKind::TimedOut,
    );
  }
}
