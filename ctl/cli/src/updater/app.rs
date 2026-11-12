use super::arguments::AppCommand;
use ctl_client::installation::app_directory;
use ctl_client::remote_bundle::desktop::{self, DesktopSigning, DesktopTarget};
use std::{io, path::Path};

mod activation;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub(super) async fn run(home: &Path, command: AppCommand) -> io::Result<()> {
  let target = DesktopTarget::native()?;
  #[cfg(target_os = "macos")]
  let app_name = macos::APP_NAME;
  #[cfg(target_os = "linux")]
  let app_name = "ctmux.AppImage";
  let destination = app_directory(home, false)?.join(app_name);
  let installed = activation::existing(&destination)?;

  #[cfg(target_os = "macos")]
  let installed_version = installed
    .as_ref()
    .map(|_| macos::installed_version(&destination))
    .transpose()?;
  #[cfg(target_os = "linux")]
  if installed.is_some() {
    linux::verify_existing(&destination)?;
  }
  match (command, installed.is_some()) {
    (AppCommand::Install, true) => {
      println!(
        "ctmux is already installed at {}. Use ctlup app update to update it.",
        destination.display()
      );
      return Ok(());
    }
    (AppCommand::Update, false) => {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        "ctmux is not installed. Run ctlup app install first.",
      ));
    }
    _ => {}
  }

  let release = desktop::latest(target).await?;
  let signing = release.signing();
  if installed.is_some() {
    #[cfg(target_os = "macos")]
    let current = installed_version
      .as_deref()
      .map(|version| is_current(&destination, version, &release.bundle_id, signing))
      .transpose()?
      .unwrap_or(false);
    #[cfg(target_os = "linux")]
    let current = linux::is_executable(&destination)? && release.matches(&destination)?;
    if current {
      println!(
        "ctmux {} is already installed at {}.",
        release.bundle_id,
        destination.display()
      );
      return Ok(());
    }
    #[cfg(target_os = "macos")]
    require_app_closed(&destination)?;
  }

  app_directory(home, true)?;
  let mut workspace = activation::Workspace::begin(&destination).map_err(|error| {
    io::Error::new(
      error.kind(),
      format!(
        "could not stage an app at {}: {error}",
        destination.display()
      ),
    )
  })?;
  let download = workspace.staging().join(release.asset_name());
  let signing_note = if signing == DesktopSigning::Unsigned {
    " (unsigned macOS release)"
  } else {
    ""
  };
  println!("Downloading ctmux {}{signing_note}…", release.bundle_id);
  release.download(&download).await?;
  #[cfg(target_os = "macos")]
  let staged = macos::stage(&download, workspace.staging(), &release.bundle_id, signing)?;
  #[cfg(target_os = "linux")]
  let staged = linux::prepare(download)?;

  #[cfg(target_os = "macos")]
  if installed.is_some() {
    require_app_closed(&destination)?;
  }
  let previous = workspace.activate(&staged, installed.as_ref())?;
  let verb = match command {
    AppCommand::Install => "Installed",
    AppCommand::Update => "Updated",
  };
  println!(
    "{verb} ctmux {} at {}.",
    release.bundle_id,
    destination.display()
  );
  if let Some(previous) = previous {
    println!("Previous app saved at {}.", previous.display());
  }
  Ok(())
}

#[cfg(target_os = "macos")]
fn is_current(
  app: &Path,
  installed: &str,
  available: &str,
  signing: DesktopSigning,
) -> io::Result<bool> {
  let installed = semver::Version::parse(installed).map_err(io::Error::other)?;
  let available_version = semver::Version::parse(available).map_err(io::Error::other)?;
  if installed.cmp_precedence(&available_version).is_gt() {
    return Err(io::Error::other(format!(
      "ctmux {installed} is newer than the Stable release {available}"
    )));
  }
  Ok(installed == available_version && macos::verify_existing(app, available, signing).is_ok())
}

#[cfg(target_os = "macos")]
fn require_app_closed(app: &Path) -> io::Result<()> {
  if macos::is_running(app)? {
    return Err(io::Error::other(
      "Quit the ctmux desktop app, then run ctlup app update again.",
    ));
  }
  Ok(())
}
