use clap::{Subcommand, ValueEnum};
use ctl_core::bundles::{Purpose, Source};
use std::io;
use std::path::PathBuf;

mod list;
mod maintenance;
mod update;
pub use update::UpdatePackage;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Usage {
  Local,
  Upload,
}
impl From<Usage> for Purpose {
  fn from(value: Usage) -> Self {
    match value {
      Usage::Local => Self::Local,
      Usage::Upload => Self::Upload,
    }
  }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Origin {
  Ci,
  Release,
}
impl From<Origin> for Source {
  fn from(value: Origin) -> Self {
    match value {
      Origin::Ci => Self::Ci,
      Origin::Release => Self::Release,
    }
  }
}

#[derive(Debug, Subcommand)]
pub enum Command {
  /// Inspect running owners and installed replacements without starting services.
  Status {
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
  /// Update local and/or saved SSH hosts, preserving running services.
  Update {
    /// Saved host names, IDs or SSH aliases (repeat or separate with commas).
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    hosts: Vec<String>,
    /// Include the local host; it is the default when no host is supplied.
    #[arg(long)]
    local: bool,
    #[arg(long, value_enum, default_value = "full-bundle")]
    package: UpdatePackage,
    /// Complete build directory or archive; defaults to each target's selection.
    #[arg(long)]
    from: Option<PathBuf>,
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
  List {
    /// Filter by target; defaults to all supported targets.
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    json: bool,
  },
  /// Import a complete build and select it, preserving every running service.
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
  method: Option<&str>,
  platform: Option<crate::RemotePlatform>,
) -> io::Result<()> {
  match command {
    Command::Status { json } => return maintenance::status(host, method, platform, json).await,
    Command::Restart {
      component,
      yes,
      dry_run,
      json,
    } => return maintenance::restart(component, host, method, platform, yes, dry_run, json).await,
    _ => {}
  }
  let home = dirs::home_dir().ok_or_else(|| io::Error::other("home directory is unavailable"))?;
  match command {
    Command::Status { .. } | Command::Restart { .. } => unreachable!(),
    Command::Update {
      hosts,
      local,
      package,
      from,
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
          source: from.map_or(
            ctl_client::component_update::BuildSource::Selected,
            |path| ctl_client::component_update::BuildSource::Provided {
              path,
              local_build,
              ctld_package,
            },
          ),
        },
        update::TargetOptions {
          host,
          method,
          platform,
        },
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
