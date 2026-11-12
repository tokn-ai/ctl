use super::arguments::AppCommand;
use ctl_client::installation::app_directory;
use ctl_client::remote_bundle::desktop::{self, DesktopRelease, DesktopSigning, DesktopTarget};
use ctl_core::component_update::BundleChannel;
use std::{
  io,
  path::{Path, PathBuf},
};

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

  let release = if options.local {
    None
  } else {
    let channel = super::selected_channel(home, options.channel)?;
    Some(desktop::latest(channel, target).await?)
  };
  #[cfg(target_os = "linux")]
  if let Some(release) = &release
    && installed.is_some()
    && linux::is_executable(&destination)?
    && release.matches(&destination)?
  {
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
  let prepared = prepare(release, workspace.staging()).await?;

  #[cfg(target_os = "macos")]
  if installed.is_some() {
    if macos::matches(&destination, &prepared.payload)? {
      return report_current(home, explicit_channel, &prepared.bundle_id, &destination);
    }
    require_app_closed(&destination)?;
  }
  #[cfg(target_os = "linux")]
  if installed.is_some() && linux::matches(&destination, &prepared.payload)? {
    return report_current(home, explicit_channel, &prepared.bundle_id, &destination);
  }
  let previous = workspace.activate(&prepared.payload, installed.as_ref())?;
  let verb = if installed.is_some() {
    "Updated"
  } else {
    "Installed"
  };
  println!(
    "{verb} ctmux {} at {}.",
    prepared.bundle_id,
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

struct PreparedApp {
  payload: PathBuf,
  bundle_id: String,
}

async fn prepare(release: Option<DesktopRelease>, staging: &Path) -> io::Result<PreparedApp> {
  let Some(release) = release else {
    eprintln!("Building the ctmux desktop app from local source…");
    let built = ctl_client::development::build_desktop(
      &std::env::current_dir()?,
      &staging.join("local-build"),
    )
    .await?;
    #[cfg(target_os = "macos")]
    let payload = macos::stage_local(&built.archive, staging, &built.app_version)?;
    #[cfg(target_os = "linux")]
    let payload = {
      let destination = staging.join("ctmux.AppImage");
      std::fs::rename(&built.archive, &destination)?;
      linux::prepare(destination)?
    };
    return Ok(PreparedApp {
      payload,
      bundle_id: built.bundle_id,
    });
  };
  let signing = release.signing();
  let download = staging.join(release.asset_name());
  let signing_note = if signing == DesktopSigning::Unsigned {
    " (unsigned macOS release)"
  } else {
    ""
  };
  println!("Downloading ctmux {}{signing_note}…", release.bundle_id);
  release.download(&download).await?;
  #[cfg(target_os = "macos")]
  let staged = macos::stage(&download, staging, &release.app_version, signing)?;
  #[cfg(target_os = "linux")]
  let staged = linux::prepare(download)?;

  Ok(PreparedApp {
    payload: staged,
    bundle_id: release.bundle_id,
  })
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

#[cfg(test)]
mod tests;
