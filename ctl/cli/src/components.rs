use clap::{Subcommand, ValueEnum};
use ctl_core::bundles::{Purpose, Source, Store};
use std::io;
use std::path::PathBuf;

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
  /// List complete stored builds and explicit selections without connecting.
  List {
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
  /// Select a previously imported complete build; never restart a service.
  Select {
    bundle_id: String,
    #[arg(long)]
    target: Option<String>,
    #[arg(long, value_enum, default_value = "upload")]
    purpose: Usage,
  },
}

pub async fn run(command: Command) -> io::Result<()> {
  let home = dirs::home_dir().ok_or_else(|| io::Error::other("home directory is unavailable"))?;
  let store = Store::new(&home);
  match command {
    Command::List { target, json } => {
      list(
        &store,
        target
          .as_deref()
          .unwrap_or_else(|| ctl_core::paths::native_target()),
        json,
      )?;
    }
    Command::Sync {
      from,
      target,
      purpose,
      local_build,
      ctld_package,
      source,
      json,
    } => {
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
        ctl_client::components::import_local(&home, &from, ctld_package.as_deref()).await?
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
        ctl_client::components::import_remote(&home, &candidate, &target, source)
          .map_err(io::Error::other)?
      };
      ctl_client::components::select(&home, purpose.into(), &bundle).await?;
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
    }
    Command::Select {
      bundle_id,
      target,
      purpose,
    } => {
      let target = target.unwrap_or_else(|| default_target(purpose, false));
      let bundle = store.get(&target, &bundle_id)?;
      ctl_client::components::select(&home, purpose.into(), &bundle).await?;
      println!("Selected {bundle_id} for {purpose:?}; running services were preserved.");
    }
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

fn list(store: &Store, target: &str, json: bool) -> io::Result<()> {
  let local = store
    .selected(Purpose::Local, target)?
    .map(|bundle| bundle.manifest.bundle_id);
  let upload = store
    .selected(Purpose::Upload, target)?
    .map(|bundle| bundle.manifest.bundle_id);
  let bundles = store.list(target)?;
  if json {
    println!("{}", serde_json::to_string(&serde_json::json!({"target_triple":target,"selected_local":local,"selected_upload":upload,"bundles":bundles.iter().map(|bundle| &bundle.manifest).collect::<Vec<_>>()})).map_err(io::Error::other)?);
  } else {
    for bundle in bundles {
      let id = &bundle.manifest.bundle_id;
      println!(
        "{id} {:?} {}{}{}",
        bundle.manifest.source,
        bundle.manifest.components["ctl-agent"].build.version,
        if local.as_ref() == Some(id) {
          " [local]"
        } else {
          ""
        },
        if upload.as_ref() == Some(id) {
          " [upload]"
        } else {
          ""
        }
      );
    }
  }
  Ok(())
}
