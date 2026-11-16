use super::arguments::AppCommand;
use ctl_client::cli::Channel;
use ctl_client::desktop_storage::app_directory;
use ctl_client::remote_bundle::desktop::{self, DesktopRelease, DesktopSigning, DesktopTarget};
use ctl_core::component_update::BundleChannel;
use serde::Serialize;
use std::{
  io::{self, Write as _},
  path::{Path, PathBuf},
};

#[cfg(unix)]
mod activation;
#[cfg(windows)]
#[path = "app/windows_activation.rs"]
mod activation;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod portable;

pub(super) async fn run(home: &Path, command: AppCommand) -> io::Result<()> {
  let (options, update) = match command {
    AppCommand::Install(options) => (options, false),
    AppCommand::Update(options) => (options, true),
  };
  install(home, Source::Published(options.channel), update)
    .await?
    .report(false)
}

pub(super) async fn install_development(
  home: &Path,
  workspace: &Path,
  json: bool,
) -> io::Result<()> {
  install(home, Source::Development(workspace), false)
    .await?
    .report(json)
}

enum Source<'a> {
  Published(Option<Channel>),
  Development(&'a Path),
}

async fn install(home: &Path, source: Source<'_>, update: bool) -> io::Result<InstalledApp> {
  let target = DesktopTarget::native()?;
  #[cfg(target_os = "macos")]
  let app_name = macos::APP_NAME;
  #[cfg(target_os = "linux")]
  let app_name = "ctmux.AppImage";
  #[cfg(windows)]
  let app_name = "ctmux";
  app_directory(home, true)?;
  let destination = app_directory(home, false)?.join(app_name);
  // Recover interrupted Windows replacements and acquire the destination lock
  // before inspecting its state or spending time building/downloading a package.
  let mut workspace = activation::Workspace::begin(&destination)?;
  let installed = activation::existing(&destination)?;

  #[cfg(target_os = "macos")]
  if installed.is_some() {
    macos::installed_version(&destination)?;
  }
  #[cfg(target_os = "linux")]
  if installed.is_some() {
    linux::verify_existing(&destination)?;
  }
  #[cfg(windows)]
  if installed.is_some() {
    portable::verify_existing(&destination, target.triple())?;
  }
  if update && installed.is_none() {
    return Err(io::Error::new(
      io::ErrorKind::NotFound,
      "ctmux is not installed. Run ctlup app install first.",
    ));
  }

  let explicit_channel = match source {
    Source::Published(channel) => channel.map(Into::into),
    Source::Development(_) => None,
  };
  let prepared = match source {
    Source::Published(channel) => {
      let channel = super::selected_channel(home, channel)?;
      let release = desktop::latest(channel, target).await?;
      #[cfg(target_os = "linux")]
      if installed.is_some()
        && linux::is_executable(&destination)?
        && release.matches(&destination)?
      {
        return already_installed(home, explicit_channel, release.bundle_id, destination);
      }
      prepare_published(release, workspace.staging()).await?
    }
    Source::Development(source) => prepare_development(source, workspace.staging()).await?,
  };

  #[cfg(target_os = "macos")]
  if installed.is_some() {
    if macos::matches(&destination, &prepared.payload)? {
      return already_installed(home, explicit_channel, prepared.bundle_id, destination);
    }
    require_app_closed(&destination)?;
  }
  #[cfg(target_os = "linux")]
  if installed.is_some() && linux::matches(&destination, &prepared.payload)? {
    return already_installed(home, explicit_channel, prepared.bundle_id, destination);
  }
  #[cfg(windows)]
  if installed.is_some() && portable::matches(&destination, &prepared.payload)? {
    return already_installed(home, explicit_channel, prepared.bundle_id, destination);
  }
  let previous = workspace.activate(&prepared.payload, installed.as_ref())?;
  let action = if installed.is_some() {
    Action::Updated
  } else {
    Action::Installed
  };
  let result = InstalledApp {
    action,
    bundle_id: prepared.bundle_id,
    destination,
    previous,
  };
  remember_channel(home, explicit_channel).map_err(|error| {
    io::Error::new(
      error.kind(),
      format!("app installed successfully, but could not save its channel preference: {error}"),
    )
  })?;
  Ok(result)
}

struct PreparedApp {
  payload: PathBuf,
  bundle_id: String,
}

async fn prepare_development(workspace: &Path, staging: &Path) -> io::Result<PreparedApp> {
  eprintln!("Building the ctmux desktop app from local source…");
  let built =
    ctl_client::development::build_desktop(workspace, &staging.join("local-build")).await?;
  #[cfg(target_os = "macos")]
  let payload = macos::stage_local(&built.archive, staging, &built.app_version)?;
  #[cfg(target_os = "linux")]
  let payload = {
    let destination = staging.join("ctmux.AppImage");
    std::fs::rename(&built.archive, &destination)?;
    linux::prepare(destination)?
  };
  #[cfg(windows)]
  let payload = portable::stage(
    &built.archive,
    staging,
    &built.app_version,
    DesktopTarget::native()?.triple(),
  )?;
  Ok(PreparedApp {
    payload,
    bundle_id: built.bundle_id,
  })
}

async fn prepare_published(release: DesktopRelease, staging: &Path) -> io::Result<PreparedApp> {
  let signing = release.signing();
  let download = staging.join(release.asset_name());
  let signing_note = if signing == DesktopSigning::Unsigned {
    " (unsigned macOS release)"
  } else {
    ""
  };
  eprintln!("Downloading ctmux {}{signing_note}…", release.bundle_id);
  release.download(&download).await?;
  #[cfg(target_os = "macos")]
  let staged = macos::stage(&download, staging, &release.app_version, signing)?;
  #[cfg(target_os = "linux")]
  let staged = linux::prepare(download)?;
  #[cfg(windows)]
  let staged = portable::stage(
    &download,
    staging,
    &release.app_version,
    DesktopTarget::native()?.triple(),
  )?;

  Ok(PreparedApp {
    payload: staged,
    bundle_id: release.bundle_id,
  })
}

fn already_installed(
  home: &Path,
  channel: Option<BundleChannel>,
  bundle_id: String,
  destination: PathBuf,
) -> io::Result<InstalledApp> {
  remember_channel(home, channel)?;
  Ok(InstalledApp {
    action: Action::AlreadyInstalled,
    bundle_id,
    destination,
    previous: None,
  })
}

fn remember_channel(home: &Path, channel: Option<BundleChannel>) -> io::Result<()> {
  if let Some(channel) = channel {
    ctl_client::desktop_storage::set_channel(home, channel)?;
  }
  Ok(())
}

#[derive(Serialize)]
struct InstalledApp {
  action: Action,
  bundle_id: String,
  destination: PathBuf,
  previous: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
  Installed,
  Updated,
  AlreadyInstalled,
}

impl InstalledApp {
  fn report(&self, json: bool) -> io::Result<()> {
    let mut output = io::stdout().lock();
    if json {
      serde_json::to_writer(&mut output, self).map_err(io::Error::other)?;
      return writeln!(output);
    }
    let destination = self.destination.display();
    match self.action {
      Action::Installed => writeln!(
        output,
        "Installed ctmux {} at {destination}.",
        self.bundle_id
      )?,
      Action::Updated => writeln!(output, "Updated ctmux {} at {destination}.", self.bundle_id)?,
      Action::AlreadyInstalled => {
        writeln!(
          output,
          "ctmux {} is already installed at {destination}.",
          self.bundle_id
        )?;
      }
    }
    if let Some(previous) = &self.previous {
      writeln!(output, "Previous app saved at {}.", previous.display())?;
    }
    Ok(())
  }
}

#[cfg(target_os = "macos")]
fn require_app_closed(app: &Path) -> io::Result<()> {
  if macos::is_running(app)? {
    return Err(io::Error::other(
      "Quit the ctmux desktop app, then retry this command.",
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests;
