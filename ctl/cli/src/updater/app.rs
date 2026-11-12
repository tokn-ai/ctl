use super::arguments::AppCommand;
use ctl_client::installation::app_directory;
use ctl_client::remote_bundle::desktop::{self, DesktopSigning, DesktopTarget};
use ctl_core::component_update::BundleChannel;
use std::{io, path::Path};

mod activation;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub(super) async fn run(home: &Path, command: AppCommand) -> io::Result<()> {
  let (options, update) = match command {
    AppCommand::Install(options) => (options, false),
    AppCommand::Update(options) => (options, true),
  };
  // App commands always target this account. --local makes that explicit.
  let _ = options.local;
  let channel = super::selected_channel(home, options.channel)?;
  let explicit_channel = options.channel.map(Into::into);
  let target = DesktopTarget::native()?;
  #[cfg(target_os = "macos")]
  let app_name = macos::APP_NAME;
  #[cfg(target_os = "linux")]
  let app_name = "ctmux.AppImage";
  let destination = app_directory(home, false)?.join(app_name);
  let installed = activation::existing(&destination)?;

  #[cfg(target_os = "macos")]
  if installed.is_some() {
    macos::installed_version(&destination)?;
  }
  #[cfg(target_os = "linux")]
  if installed.is_some() {
    linux::verify_existing(&destination)?;
  }
  if update && installed.is_none() {
    return Err(io::Error::new(
      io::ErrorKind::NotFound,
      "ctmux is not installed. Run ctlup app install first.",
    ));
  }

  let release = desktop::latest(channel, target).await?;
  let signing = release.signing();
  #[cfg(target_os = "linux")]
  if installed.is_some() && linux::is_executable(&destination)? && release.matches(&destination)? {
    return report_current(home, explicit_channel, &release.bundle_id, &destination);
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
  let staged = macos::stage(
    &download,
    workspace.staging(),
    &release.app_version,
    signing,
  )?;
  #[cfg(target_os = "linux")]
  let staged = linux::prepare(download)?;

  #[cfg(target_os = "macos")]
  if installed.is_some() {
    if macos::matches(&destination, &staged)? {
      return report_current(home, explicit_channel, &release.bundle_id, &destination);
    }
    require_app_closed(&destination)?;
  }
  let previous = workspace.activate(&staged, installed.as_ref())?;
  let verb = if installed.is_some() {
    "Updated"
  } else {
    "Installed"
  };
  println!(
    "{verb} ctmux {} at {}.",
    release.bundle_id,
    destination.display()
  );
  if let Some(previous) = previous {
    println!("Previous app saved at {}.", previous.display());
  }
  if let Some(channel) = explicit_channel {
    ctl_client::installation::set_channel(home, channel).map_err(|error| {
      io::Error::new(
        error.kind(),
        format!("app installed successfully, but could not save its channel preference: {error}"),
      )
    })?;
  }
  Ok(())
}

fn report_current(
  home: &Path,
  channel: Option<BundleChannel>,
  bundle_id: &str,
  destination: &Path,
) -> io::Result<()> {
  if let Some(channel) = channel {
    ctl_client::installation::set_channel(home, channel)?;
  }
  println!(
    "ctmux {bundle_id} is already installed at {}.",
    destination.display()
  );
  Ok(())
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
