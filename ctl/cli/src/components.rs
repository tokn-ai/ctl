use clap::Subcommand;
use clap::ValueEnum;
#[cfg(unix)]
use ctl_core::bundles::{Purpose, Source};
use std::io;
#[cfg(unix)]
use std::path::PathBuf;

mod check;
#[cfg(unix)]
mod list;
mod maintenance;
#[cfg(unix)]
mod update;
#[cfg(unix)]
pub use update::UpdatePackage;

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
impl From<Usage> for Purpose {
  fn from(value: Usage) -> Self {
    match value {
      Usage::Local => Self::Local,
      Usage::Upload => Self::Upload,
    }
  }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Origin {
  Ci,
  Release,
}
#[cfg(unix)]
impl From<Origin> for Source {
  fn from(value: Origin) -> Self {
    match value {
      Origin::Ci => Self::Ci,
      Origin::Release => Self::Release,
    }
  }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Channel {
  Dev,
  Stable,
}
impl From<Channel> for ctl_core::component_update::BundleChannel {
  fn from(value: Channel) -> Self {
    match value {
      Channel::Dev => Self::Dev,
      Channel::Stable => Self::Stable,
    }
  }
}

#[derive(Debug, Subcommand)]
pub enum Command {
  /// Check published channel metadata without downloading or installing components.
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
  List {
    /// Filter by target; defaults to all supported targets.
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    json: bool,
  },
  /// Import a complete build and select it, preserving every running service.
  #[cfg(unix)]
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
    Command::Check { channel, json } => check::run(channel, json).await,
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
    command => run_bundle(command, target_options).await,
  }
}

#[cfg(unix)]
async fn run_bundle(command: Command, target_options: TargetOptions<'_>) -> io::Result<()> {
  let home = dirs::home_dir().ok_or_else(|| io::Error::other("home directory is unavailable"))?;
  match command {
    Command::Check { .. }
    | Command::Status { .. }
    | Command::Restart { .. }
    | Command::Stop { .. } => unreachable!(),
    Command::Update {
      hosts,
      local,
      package,
      from,
      channel,
      selected,
      local_build,
      ctld_package,
      json,
    } => {
      return update::run(
        &home,
        hosts,
        local,
        ctl_client::component_update::UpdateOptions {
          package: package.into(),
          source: source_with_bundle_override(
            from.map_or_else(
              || {
                channel.map_or_else(
                  || {
                    if selected {
                      ctl_client::component_update::BuildSource::Selected
                    } else {
                      ctl_client::component_update::BuildSource::Automatic
                    }
                  },
                  |channel| ctl_client::component_update::BuildSource::Published {
                    channel: channel.into(),
                  },
                )
              },
              |path| ctl_client::component_update::BuildSource::Provided {
                path,
                local_build,
                ctld_package,
              },
            ),
            std::env::var_os("CTL_REMOTE_BUNDLES_DIR").map(PathBuf::from),
          ),
        },
        target_options,
        json,
      )
      .await;
    }
    Command::List { target, json } => {
      let directories = crate::remote::update_bundle_directories()?;
      list::run(&home, target.as_deref(), &directories, json).await?;
    }
    command @ Command::Sync { .. } => sync(&home, command).await?,
    Command::Select {
      bundle_id,
      target,
      purpose,
    } => {
      let target = target.unwrap_or_else(|| default_target(purpose, false));
      let directories = crate::remote::update_bundle_directories()?;
      let bundle = {
        let home = home.clone();
        let id = bundle_id.clone();
        tokio::task::spawn_blocking(move || {
          ctl_client::components::inventory::load_selection(&home, &directories, &target, &id)
        })
        .await
        .map_err(io::Error::other)??
      };
      ctl_client::components::select(&home, purpose.into(), &bundle).await?;
      println!("Selected {bundle_id} for {purpose:?}; running services were preserved.");
    }
  }
  Ok(())
}

#[cfg(unix)]
pub(crate) fn source_with_bundle_override(
  source: ctl_client::component_update::BuildSource,
  directory: Option<PathBuf>,
) -> ctl_client::component_update::BuildSource {
  use ctl_client::component_update::BuildSource;
  match (source, directory) {
    (BuildSource::Automatic, Some(path)) => BuildSource::Provided {
      path,
      local_build: false,
      ctld_package: None,
    },
    (source, _) => source,
  }
}

#[cfg(all(test, unix))]
mod source_tests {
  use super::*;
  use ctl_client::component_update::{BuildSource, BundleChannel};

  #[test]
  fn environment_bundle_override_applies_only_to_automatic_updates() {
    let directory = PathBuf::from("/provided-bundles");
    assert!(matches!(
      source_with_bundle_override(BuildSource::Automatic, Some(directory.clone())),
      BuildSource::Provided { path, local_build: false, ctld_package: None } if path == directory
    ));
    assert!(matches!(
      source_with_bundle_override(BuildSource::Automatic, None),
      BuildSource::Automatic
    ));
    for source in [
      BuildSource::Selected,
      BuildSource::Published {
        channel: BundleChannel::Dev,
      },
      BuildSource::Published {
        channel: BundleChannel::Stable,
      },
      BuildSource::Provided {
        path: PathBuf::from("/explicit-build"),
        local_build: true,
        ctld_package: Some(PathBuf::from("/signed-ctld")),
      },
    ] {
      let expected = serde_json::to_value(&source).unwrap();
      let resolved = source_with_bundle_override(source, Some(directory.clone()));
      assert_eq!(serde_json::to_value(resolved).unwrap(), expected);
    }
  }
}

#[cfg(unix)]
async fn sync(home: &std::path::Path, command: Command) -> io::Result<()> {
  let Command::Sync {
    from,
    target,
    purpose,
    local_build,
    ctld_package,
    source,
    json,
  } = command
  else {
    unreachable!()
  };
  let target = target.unwrap_or_else(|| default_target(purpose, local_build));
  let bundle = if local_build {
    if target != ctl_core::paths::native_target() {
      return Err(io::Error::other(
        "local build metadata can only be queried for the native target",
      ));
    }
    if cfg!(target_os = "macos") && matches!(purpose, Usage::Local) && ctld_package.is_none() {
      return Err(io::Error::other(
        "local macOS sync requires --ctld-package with a complete signed ctld.app and its receipt",
      ));
    }
    ctl_client::components::import_local(home, &from, ctld_package.as_deref()).await?
  } else {
    let candidate = ctl_client::remote_bundle::read_compatible_bundle(&[from], &target)
      .map_err(io::Error::other)?
      .ok_or_else(|| io::Error::other("no compatible complete bundle was found"))?;
    let source = source.map_or_else(
      || {
        if candidate.bundle_id == candidate.app_version {
          Source::Release
        } else {
          Source::Ci
        }
      },
      Source::from,
    );
    ctl_client::components::import_remote(home, &candidate, &target, source)
      .map_err(io::Error::other)?
  };
  ctl_client::components::select(home, purpose.into(), &bundle).await?;
  if json {
    println!(
      "{}",
      serde_json::to_string(&bundle.manifest).map_err(io::Error::other)?
    );
  } else {
    println!(
      "Selected complete bundle {} for {purpose:?}. Running services were preserved; restart separately.",
      bundle.manifest.bundle_id
    );
  }

  Ok(())
}

#[cfg(unix)]
fn default_target(purpose: Usage, local_build: bool) -> String {
  let native = ctl_core::paths::native_target();
  if matches!(purpose, Usage::Upload) && !local_build {
    native.strip_suffix("-unknown-linux-gnu").map_or_else(
      || native.into(),
      |arch| format!("{arch}-unknown-linux-musl"),
    )
  } else {
    native.into()
  }
}
