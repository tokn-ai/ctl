use clap::Subcommand;
use clap::ValueEnum;
use std::io;
#[cfg(unix)]
use std::path::PathBuf;

mod maintenance;
#[cfg(unix)]
pub use crate::updater::update::UpdatePackage;

#[derive(Clone, Copy)]
pub(crate) struct TargetOptions<'a> {
  host: Option<&'a str>,
  destination: Option<&'a str>,
  method: Option<&'a str>,
  platform: Option<crate::RemotePlatform>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Usage {
  Local,
  Upload,
}
#[cfg(unix)]
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Origin {
  Ci,
  Release,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Channel {
  #[value(alias = "dev")]
  Nightly,
  Stable,
}
impl From<Channel> for ctl_core::component_update::BundleChannel {
  fn from(value: Channel) -> Self {
    match value {
      Channel::Nightly => Self::Nightly,
      Channel::Stable => Self::Stable,
    }
  }
}

#[derive(Debug, Subcommand)]
pub enum Command {
  /// Check published channel metadata without downloading or installing components.
  #[command(hide = true)]
  Check {
    #[arg(long, value_enum)]
    channel: Option<Channel>,
    #[arg(long)]
    json: bool,
  },
  /// Inspect running owners and installed replacements without starting services.
  Status {
    #[arg(long)]
    json: bool,
  },
  /// Cooperatively stop an existing daemon without launching a replacement.
  Stop {
    #[arg(value_enum)]
    component: maintenance::Daemon,
    #[arg(long, conflicts_with = "dry_run")]
    yes: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
  },
  /// Cooperatively restart a daemon after verifying its replacement.
  Restart {
    #[arg(value_enum)]
    component: maintenance::Daemon,
    /// Approve the displayed impact without an interactive prompt.
    #[arg(long, conflicts_with = "dry_run")]
    yes: bool,
    /// Verify and display the restart plan without changing any service.
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
  },
  /// Update local or remote hosts, preserving running services.
  #[cfg(unix)]
  #[command(hide = true)]
  Update {
    /// Saved host names or IDs (repeat or separate with commas).
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    hosts: Vec<String>,
    /// Include the local host; it is the default when no host is supplied.
    #[arg(long)]
    local: bool,
    #[arg(long, value_enum, default_value = "full-bundle")]
    package: UpdatePackage,
    /// Complete build directory or archive instead of automatic source resolution.
    #[arg(long, conflicts_with_all = ["channel", "selected"])]
    from: Option<PathBuf>,
    /// Fetch this publication channel, bypassing a development checkout build.
    #[arg(long, value_enum, conflicts_with_all = ["from", "selected"])]
    channel: Option<Channel>,
    /// Explicitly use the existing offline selection for each target.
    #[arg(long, conflicts_with_all = ["from", "channel"])]
    selected: bool,
    /// Snapshot four native binaries from --from before installing.
    #[arg(long, requires = "from")]
    local_build: bool,
    /// Signed ctld.app package and receipt for local macOS full updates.
    #[arg(long, requires = "local_build")]
    ctld_package: Option<PathBuf>,
    #[arg(long)]
    json: bool,
  },
  /// List included and stored complete builds and selections without connecting.
  #[cfg(unix)]
  #[command(hide = true)]
  List {
    /// Filter by target; defaults to all supported targets.
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    json: bool,
  },
  /// Import a complete build and select it, preserving every running service.
  #[cfg(unix)]
  #[command(hide = true)]
  Sync {
    /// Directory containing a verified CI/release bundle-set or four native binaries.
    #[arg(long)]
    from: PathBuf,
    #[arg(long)]
    target: Option<String>,
    #[arg(long, value_enum, default_value = "upload")]
    purpose: Usage,
    /// Query and snapshot an explicitly chosen native local build.
    #[arg(long, conflicts_with = "source")]
    local_build: bool,
    /// Complete signed ctld.app package from ctl setup, for local macOS builds.
    #[arg(long, requires = "local_build")]
    ctld_package: Option<PathBuf>,
    #[arg(long, value_enum)]
    source: Option<Origin>,
    #[arg(long)]
    json: bool,
  },
  /// Select a stored or included complete build; never restart a service.
  #[cfg(unix)]
  #[command(hide = true)]
  Select {
    bundle_id: String,
    #[arg(long)]
    target: Option<String>,
    #[arg(long, value_enum, default_value = "upload")]
    purpose: Usage,
  },
}

pub async fn run(
  command: Command,
  host: Option<&str>,
  destination: Option<&str>,
  method: Option<&str>,
  platform: Option<crate::RemotePlatform>,
) -> io::Result<()> {
  let target_options = TargetOptions {
    host,
    destination,
    method,
    platform,
  };
  match command {
    Command::Check { .. } => Err(updater_required()),
    Command::Status { json } => maintenance::status(target_options, json).await,
    Command::Stop {
      component,
      yes,
      dry_run,
      json,
    } => maintenance::stop(component, target_options, yes, dry_run, json).await,
    Command::Restart {
      component,
      yes,
      dry_run,
      json,
    } => maintenance::restart(component, target_options, yes, dry_run, json).await,
    #[cfg(unix)]
    _ => Err(updater_required()),
  }
}

fn updater_required() -> io::Error {
  io::Error::other(
    "Installation management moved to ctlup. Use ctlup install, update, status or rollback; ctl components manages running services.",
  )
}
