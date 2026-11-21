use ctl_client::cli::Channel;
use ctl_client::remote_bundle::desktop::{self, DesktopRelease, DesktopSigning, DesktopTarget};
use ctl_client::remote_bundle::published;
use ctl_core::component_update::BundleChannel;
use ctl_core::installations::{DesktopSelection, DesktopSource, Installations};
use ctl_core::private_fs;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
  fs::File,
  io::{self, Read as _, Write as _},
  path::{Path, PathBuf},
};

mod activation;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod portable;
#[cfg(windows)]
mod shortcut;

const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;

pub(super) async fn install(
  home: &Path,
  channel: Option<Channel>,
  release: Option<&str>,
  pr: Option<u64>,
  from: Option<&Path>,
  json: bool,
) -> io::Result<()> {
  let work = activation::Workspace::begin(home)?;
  let current = work.installations().desktop_selection()?;
  let target = DesktopTarget::native()?;
  let (package, source) = if let Some(path) = from {
    let metadata = DesktopMetadata::provided(target);
    (
      prepare_supplied(&work, path, metadata)?,
      DesktopSource::Provided,
    )
  } else if let Some(number) = pr {
    let release = desktop::pull_request(number, target, work.staging()).await?;
    (
      prepare_published(&work, release, target).await?,
      DesktopSource::PullRequest { number },
    )
  } else if let Some(publication_id) = release {
    let publication = published::view_retained_release(publication_id)
      .await
      .map_err(io::Error::other)?
      .release;
    let release = desktop::resolve(publication, target, work.staging()).await?;
    (
      prepare_published(&work, release, target).await?,
      DesktopSource::Release {
        publication_id: publication_id.to_owned(),
      },
    )
  } else {
    let channel = channel.map_or_else(BundleChannel::configured_default, Into::into);
    let release = desktop::latest(channel, target, work.staging()).await?;
    (
      prepare_published(&work, release, target).await?,
      DesktopSource::Channel {
        channel: channel.name().to_owned(),
      },
    )
  };
  activate(&work, current.as_ref(), &package, source, json)
}

pub(super) async fn update(home: &Path, check: bool, json: bool) -> io::Result<()> {
  let installations = Installations::new(home);
  let Some(selected) = installations.desktop_selection()? else {
    if check {
      return report_inactive_check(app_status(home, None)?, CheckState::Uninstalled, json);
    }
    return Err(io::Error::new(
      io::ErrorKind::NotFound,
      "Run ctlup install app to install ctmux.",
    ));
  };
  let DesktopSource::Channel { channel } = &selected.source else {
    let installed = app_status(home, Some(selected.clone()))?;
    if check || !installed.missing {
      let state = if installed.missing {
        CheckState::Missing
      } else {
        CheckState::Pinned
      };
      return report_inactive_check(installed, state, json);
    }
    return ensure_selected(&activation::Workspace::begin(home)?, &selected, json);
  };
  let channel = parse_channel(channel)?;
  let target = DesktopTarget::native()?;
  if check {
    return check_updates(home, selected, target, channel, json).await;
  }
  let work = activation::Workspace::begin(home)?;
  let release = desktop::latest(channel, target, work.staging()).await?;
  let package = prepare_published(&work, release, target).await?;
  let source = selected.source.clone();
  activate(&work, Some(&selected), &package, source, json)
}

fn report_inactive_check(installed: AppStatus, state: CheckState, json: bool) -> io::Result<()> {
  if json {
    return report_json(&UpdateCheck {
      installed,
      latest_bundle_id: None,
      publication_id: None,
      state,
    });
  }
  report_status(installed, false)?;
  if matches!(state, CheckState::Pinned) {
    println!("Use ctlup install app to select a new desktop source.");
  }
  Ok(())
}

pub(super) fn status(home: &Path, json: bool) -> io::Result<()> {
  report_status(
    app_status(home, Installations::new(home).desktop_selection()?)?,
    json,
  )
}

pub(super) fn with_current_launch_entry<T>(
  home: &Path,
  operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
  if Installations::new(home).desktop_selection()?.is_none() {
    return operation();
  }
  let work = activation::Workspace::begin(home)?;
  let before = work.installations().desktop_selection()?;
  if let Some(selection) = before.as_ref() {
    work.refresh(selection)?;
  }
  let result = operation()?;
  let after = work.installations().desktop_selection()?;
  if before.is_some() && after.is_none() {
    work.clear_launch_entry()?;
  }
  work.prune_downloads(after.as_ref())?;
  Ok(result)
}

pub(super) async fn install_development(
  home: &Path,
  workspace: &Path,
  json: bool,
) -> io::Result<()> {
  let work = activation::Workspace::begin(home)?;
  let current = work.installations().desktop_selection()?;
  eprintln!("Building the ctmux desktop app from local source…");
  let built =
    ctl_client::development::build_desktop(workspace, &work.staging().join("local-build")).await?;
  let mut metadata = DesktopMetadata::provided(DesktopTarget::native()?);
  metadata.app_version = Some(built.app_version);
  metadata.bundle_id = Some(built.bundle_id);
  let package = prepare_supplied(&work, &built.archive, metadata)?;
  activate(
    &work,
    current.as_ref(),
    &package,
    DesktopSource::Checkout,
    json,
  )
}

fn ensure_selected(
  work: &activation::Workspace,
  selected: &DesktopSelection,
  json: bool,
) -> io::Result<()> {
  let digest = Digest::parse(&selected.content_id)?;
  // Detect an active selection changed by another updater before refreshing
  // the projection or repairing its payload.
  if work.installations().desktop_selection()?.as_ref() != Some(selected) {
    return Err(io::Error::other(
      "the desktop selection changed; retry the operation",
    ));
  }
  let cached = work.cached(&digest)?;
  let Some(cached) = cached else {
    let payload = work.existing(&digest)?.ok_or_else(|| {
      io::Error::new(io::ErrorKind::NotFound, "The desktop payload and its verified archive are missing. Select a source with ctlup install app.")
    })?;
    let metadata = activation::read_metadata(
      payload
        .parent()
        .expect("managed desktop object has a parent"),
      &digest,
    )?;
    work.refresh(selected)?;
    return report_activation(
      work.destination(),
      &metadata,
      Action::AlreadyInstalled,
      json,
    );
  };
  activate(work, Some(selected), &cached, selected.source.clone(), json)
}

fn activate(
  work: &activation::Workspace,
  current: Option<&DesktopSelection>,
  package: &CachedPackage,
  source: DesktopSource,
  json: bool,
) -> io::Result<()> {
  if package_digest(&package.archive)? != package.metadata.content_id {
    return Err(invalid(
      "desktop archive checksum does not match its selected content ID",
    ));
  }
  let prepared = prepare_payload(work.staging(), package)?;
  work.cache(&CachedPackage {
    metadata: prepared.metadata.clone(),
    archive: package.archive.clone(),
  })?;
  let selected = DesktopSelection {
    content_id: package.metadata.content_id.0.clone(),
    source,
  };
  let reused = work.activate(&prepared, current, &selected)?;
  let action = match current {
    Some(current) if current == &selected && reused => Action::AlreadyInstalled,
    Some(_) => Action::Updated,
    None => Action::Installed,
  };
  report_activation(work.destination(), &prepared.metadata, action, json)
}

async fn check_updates(
  home: &Path,
  selected: DesktopSelection,
  target: DesktopTarget,
  channel: BundleChannel,
  json: bool,
) -> io::Result<()> {
  let current = app_status(home, Some(selected))?;
  let publication = published::view_release(channel, None)
    .await
    .map_err(io::Error::other)?
    .release;
  let newest = publication.bundle_id.clone();
  let publication_id = publication.publication_id.clone();
  let state = if channel == BundleChannel::Nightly {
    if !current.missing && current.publication_id.as_deref() == Some(publication_id.as_str()) {
      CheckState::Current
    } else if desktop::nightly_available(&publication, target).await? {
      CheckState::UpdateAvailable
    } else {
      CheckState::Unavailable
    }
  } else {
    match desktop::stable_descriptor(publication, target).await {
      Ok(release)
        if !current.missing && current.content_id.as_deref() == Some(release.content_id()) =>
      {
        CheckState::Current
      }
      Ok(_) => CheckState::UpdateAvailable,
      Err(error) if error.kind() == io::ErrorKind::NotFound => CheckState::Unavailable,
      Err(error) => return Err(error),
    }
  };
  if json {
    return report_json(&UpdateCheck {
      installed: current,
      latest_bundle_id: Some(newest),
      publication_id: Some(publication_id),
      state,
    });
  }
  match state {
    CheckState::Current => println!("ctmux is up to date on {}.", channel.name()),
    CheckState::UpdateAvailable => println!("ctmux {newest} is available on {}.", channel.name()),
    _ => println!(
      "The latest {} desktop package is unavailable for this platform.",
      channel.name()
    ),
  }
  Ok(())
}

#[derive(Serialize)]
struct UpdateCheck {
  installed: AppStatus,
  latest_bundle_id: Option<String>,
  publication_id: Option<String>,
  state: CheckState,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckState {
  Uninstalled,
  Pinned,
  Missing,
  Current,
  UpdateAvailable,
  Unavailable,
}

#[derive(Serialize)]
struct AppStatus {
  content_id: Option<String>,
  bundle_id: Option<String>,
  publication_id: Option<String>,
  source: Option<DesktopSource>,
  payload: Option<PathBuf>,
  launch_entry: PathBuf,
  missing: bool,
}

fn app_status(home: &Path, selected: Option<DesktopSelection>) -> io::Result<AppStatus> {
  let launch_entry = app_destination(home)?;
  let Some(selected) = selected else {
    return Ok(AppStatus {
      content_id: None,
      bundle_id: None,
      publication_id: None,
      source: None,
      payload: None,
      launch_entry,
      missing: false,
    });
  };
  let digest = Digest::parse(&selected.content_id)?;
  let paths = Installations::new(home);
  let store = paths.updater_root();
  let payload = activation::status_payload(&store, &digest)?;
  let missing = payload.is_none();
  let metadata = if let Some(payload) = payload.as_ref() {
    let object = payload
      .parent()
      .ok_or_else(|| invalid("managed desktop payload has no parent"))?;
    Some(activation::read_metadata(object, &digest)?)
  } else {
    activation::cached_metadata(&store, &digest)?
  };
  let (bundle_id, publication_id) = metadata
    .map(|metadata| (metadata.bundle_id, metadata.publication_id))
    .unwrap_or_default();
  Ok(AppStatus {
    content_id: Some(selected.content_id),
    bundle_id,
    publication_id,
    source: Some(selected.source),
    payload,
    launch_entry,
    missing,
  })
}

fn report_status(status: AppStatus, json: bool) -> io::Result<()> {
  if json {
    return report_json(&status);
  }
  let Some(content_id) = status.content_id else {
    println!("The ctmux desktop app is not installed.");
    return Ok(());
  };
  let build = status.bundle_id.as_deref().unwrap_or(&content_id);
  if status.missing {
    println!("ctmux {build} (missing)");
  } else {
    println!("ctmux {build} at {}.", status.launch_entry.display());
  }
  if let Some(source) = status.source {
    match source {
      DesktopSource::Channel { channel } => println!("Tracking {channel}."),
      DesktopSource::Release { publication_id } => println!("Pinned to release {publication_id}."),
      DesktopSource::PullRequest { number } => println!("Pinned to PR #{number}."),
      DesktopSource::Provided => println!("Installed from a supplied archive."),
      DesktopSource::Checkout => println!("Built from a checkout."),
    }
  }
  Ok(())
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(transparent)]
struct Digest(String);

impl Digest {
  fn parse(value: &str) -> io::Result<Self> {
    if value.len() == 64
      && value
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
      return Ok(Self(value.to_owned()));
    }
    Err(invalid("invalid desktop package content ID"))
  }

  fn as_str(&self) -> &str {
    &self.0
  }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopMetadata {
  schema_version: u16,
  content_id: Digest,
  bundle_id: Option<String>,
  app_version: Option<String>,
  publication_id: Option<String>,
  target_triple: String,
  signing: Signing,
}

impl DesktopMetadata {
  fn provided(target: DesktopTarget) -> Self {
    Self {
      schema_version: 1,
      content_id: Digest(String::new()),
      bundle_id: None,
      app_version: None,
      publication_id: None,
      target_triple: target.triple().to_owned(),
      signing: Signing::Provided,
    }
  }

  fn validate(&self, digest: &Digest) -> io::Result<()> {
    if self.schema_version != 1
      || self.content_id != *digest
      || self.target_triple != DesktopTarget::native()?.triple()
      || [&self.bundle_id, &self.app_version, &self.publication_id]
        .into_iter()
        .flatten()
        .any(|value| value.is_empty() || value.len() > 256 || value.chars().any(char::is_control))
    {
      return Err(invalid(
        "desktop package metadata does not match the stored object",
      ));
    }
    Ok(())
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Signing {
  Signed,
  Unsigned,
  NotApplicable,
  Provided,
}

impl From<DesktopSigning> for Signing {
  fn from(value: DesktopSigning) -> Self {
    match value {
      DesktopSigning::Signed => Self::Signed,
      DesktopSigning::Unsigned => Self::Unsigned,
      DesktopSigning::NotApplicable => Self::NotApplicable,
    }
  }
}

struct CachedPackage {
  archive: PathBuf,
  metadata: DesktopMetadata,
}

struct DesktopPayload {
  path: PathBuf,
  metadata: DesktopMetadata,
}

async fn prepare_published(
  work: &activation::Workspace,
  release: DesktopRelease,
  target: DesktopTarget,
) -> io::Result<CachedPackage> {
  let signing = release.signing();
  let signing_note = if signing == DesktopSigning::Unsigned {
    " (unsigned macOS release)"
  } else {
    ""
  };
  let digest = Digest::parse(release.content_id())?;
  let cached = work.cached(&digest)?;
  let verified = match cached
    .as_ref()
    .map(|package| release.matches(&package.archive))
    .transpose()
  {
    Ok(matches) => matches == Some(true),
    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
    Err(error) => return Err(error),
  };
  let archive = if let Some(cached) = cached.filter(|_| verified) {
    cached.archive
  } else {
    eprintln!("Downloading ctmux {}{signing_note}…", release.bundle_id);
    let archive = work.staging().join(release.asset_name());
    release.download(&archive).await?;
    archive
  };
  Ok(CachedPackage {
    archive,
    metadata: DesktopMetadata {
      content_id: digest,
      app_version: Some(release.app_version),
      bundle_id: Some(release.bundle_id),
      publication_id: release.publication_id,
      signing: signing.into(),
      ..DesktopMetadata::provided(target)
    },
  })
}

fn prepare_supplied(
  work: &activation::Workspace,
  path: &Path,
  mut metadata: DesktopMetadata,
) -> io::Result<CachedPackage> {
  let archive = work.staging().join("provided-package");
  copy_package(path, &archive)?;
  metadata.content_id = package_digest(&archive)?;
  Ok(CachedPackage { archive, metadata })
}

fn prepare_payload(staging: &Path, package: &CachedPackage) -> io::Result<DesktopPayload> {
  let metadata = package.metadata.clone();
  metadata.validate(&metadata.content_id)?;
  #[cfg(target_os = "macos")]
  let (path, metadata) = {
    let mut metadata = metadata;
    let path = match metadata.signing {
      Signing::Provided => {
        macos::stage_provided(&package.archive, staging, metadata.app_version.as_deref())?
      }
      Signing::Signed | Signing::Unsigned => macos::stage(
        &package.archive,
        staging,
        metadata
          .app_version
          .as_deref()
          .ok_or_else(|| invalid("desktop metadata has no version"))?,
        if matches!(metadata.signing, Signing::Signed) {
          DesktopSigning::Signed
        } else {
          DesktopSigning::Unsigned
        },
      )?,
      Signing::NotApplicable => return Err(invalid("macOS desktop package has no signing mode")),
    };
    if metadata.app_version.is_none() {
      metadata.app_version = Some(macos::installed_version(&path)?);
    }
    if metadata.bundle_id.is_none() {
      metadata.bundle_id.clone_from(&metadata.app_version);
    }
    (path, metadata)
  };
  #[cfg(target_os = "linux")]
  let path = {
    let destination = staging.join(payload_name());
    copy_package(&package.archive, &destination)?;
    linux::prepare(destination)?
  };
  #[cfg(windows)]
  let (path, metadata) = {
    let mut metadata = metadata;
    let path = portable::stage_package(
      &package.archive,
      staging,
      metadata.app_version.as_deref(),
      &metadata.target_triple,
    )?;
    if metadata.app_version.is_none() {
      metadata.app_version = Some(portable::manifest(&path)?.app_version);
    }
    if metadata.bundle_id.is_none() {
      metadata.bundle_id.clone_from(&metadata.app_version);
    }
    (path, metadata)
  };
  Ok(DesktopPayload { path, metadata })
}

fn open_package(path: &Path) -> io::Result<File> {
  let file = private_fs::options().read(true).open(path)?;
  let info = file.metadata()?;
  if !private_fs::is_owned_file(&info) || !(1..=MAX_PACKAGE_BYTES).contains(&info.len()) {
    return Err(invalid(
      "expected an owned desktop package within the size limit",
    ));
  }
  Ok(file)
}

fn copy_package(source: &Path, destination: &Path) -> io::Result<()> {
  let mut input = open_package(source)?.take(MAX_PACKAGE_BYTES + 1);
  let mut output = private_fs::options()
    .write(true)
    .create_new(true)
    .open(destination)?;
  if !(1..=MAX_PACKAGE_BYTES).contains(&io::copy(&mut input, &mut output)?) {
    return Err(invalid("desktop package exceeds the size limit"));
  }
  output.sync_all()
}

fn package_digest(path: &Path) -> io::Result<Digest> {
  let mut file = open_package(path)?.take(MAX_PACKAGE_BYTES + 1);
  let mut digest = Sha256::new();
  if !(1..=MAX_PACKAGE_BYTES).contains(&io::copy(&mut file, &mut digest)?) {
    return Err(invalid("desktop package exceeds the size limit"));
  }
  Ok(Digest(format!("{:x}", digest.finalize())))
}

fn payload_name() -> &'static str {
  #[cfg(target_os = "macos")]
  return macos::APP_NAME;
  #[cfg(target_os = "linux")]
  return "ctmux.AppImage";
  #[cfg(windows)]
  return "ctmux";
}

fn app_destination(home: &Path) -> io::Result<PathBuf> {
  let directory = ctl_client::desktop_storage::app_directory(home, false)?;
  let name = if cfg!(windows) {
    "ctmux.lnk"
  } else {
    payload_name()
  };
  Ok(directory.join(name))
}

fn payload_matches(installed: &Path, staged: &Path) -> io::Result<bool> {
  #[cfg(target_os = "macos")]
  return macos::matches(installed, staged);
  #[cfg(target_os = "linux")]
  return linux::matches(installed, staged);
  #[cfg(windows)]
  return portable::matches(installed, staged);
}

fn parse_channel(value: &str) -> io::Result<BundleChannel> {
  match value {
    "stable" => Ok(BundleChannel::Stable),
    "nightly" => Ok(BundleChannel::Nightly),
    _ => Err(invalid("invalid tracked desktop channel")),
  }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
  Installed,
  Updated,
  AlreadyInstalled,
}

#[derive(Serialize)]
struct InstalledApp<'a> {
  action: Action,
  bundle_id: Option<&'a str>,
  content_id: &'a str,
  destination: &'a Path,
}

fn report_activation(
  destination: &Path,
  metadata: &DesktopMetadata,
  action: Action,
  json: bool,
) -> io::Result<()> {
  if json {
    return report_json(&InstalledApp {
      action,
      bundle_id: metadata.bundle_id.as_deref(),
      content_id: metadata.content_id.as_str(),
      destination,
    });
  }
  let build = metadata
    .bundle_id
    .as_deref()
    .unwrap_or(metadata.content_id.as_str());
  let destination = destination.display();
  match action {
    Action::Installed => println!("Installed ctmux {build} at {destination}."),
    Action::Updated => println!("Updated ctmux {build} at {destination}."),
    Action::AlreadyInstalled => println!("ctmux {build} is already installed at {destination}."),
  }
  Ok(())
}

fn report_json(value: &impl Serialize) -> io::Result<()> {
  let mut output = io::stdout().lock();
  serde_json::to_writer(&mut output, value).map_err(io::Error::other)?;
  writeln!(output)
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
