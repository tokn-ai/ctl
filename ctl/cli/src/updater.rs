//! Installation-manager frontend; runtime commands remain in ctl and ctmux.

use clap::Parser;
use std::io;
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
mod archive;
mod arguments;
#[cfg(unix)]
mod development;
#[cfg(unix)]
mod release;
#[cfg(unix)]
mod status;
#[cfg(unix)]
pub(crate) mod update;

use arguments::{Arguments, Command};
#[cfg(unix)]
use ctl_client::remote_bundle::published;

#[cfg(unix)]
#[derive(Clone, Copy)]
pub(crate) struct TargetOptions<'a> {
  host: Option<&'a str>,
  destination: Option<&'a str>,
  method: Option<&'a str>,
  platform: Option<crate::RemotePlatform>,
}

pub(crate) fn main() {
  let arguments = Arguments::parse();
  if matches!(arguments.command, Command::ComponentInfo) {
    if let Err(error) = crate::component_info::print() {
      eprintln!("ctlup: {error}");
      std::process::exit(1);
    }
    return;
  }
  let runtime = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .unwrap_or_else(|error| {
      eprintln!("ctlup: could not start runtime: {error}");
      std::process::exit(1);
    });
  let result = runtime.block_on(run(arguments));
  drop(runtime);
  let code = match result {
    Ok(code) => code,
    Err(error) => {
      eprintln!("ctlup: {error}");
      1
    }
  };
  std::process::exit(code);
}

#[cfg(unix)]
async fn run(arguments: Arguments) -> io::Result<i32> {
  let home = dirs::home_dir().ok_or_else(|| io::Error::other("home directory is unavailable"))?;
  let target = TargetOptions {
    host: arguments.host.as_deref(),
    destination: arguments.destination.as_deref(),
    method: arguments.method.as_deref(),
    platform: None,
  };
  if target.destination.is_some() && (target.host.is_some() || target.method.is_some()) {
    return Err(io::Error::other(
      "--destination cannot be combined with --host or --method",
    ));
  }
  match arguments.command {
    Command::ComponentInfo => unreachable!(),
    Command::Install(options) | Command::Update(options) => {
      update::run(&home, options, target).await?;
    }
    Command::Status { json } => status::run(&home, target, false, json).await?,
    Command::Dev(options) => {
      require_local(target)?;
      development::run(&home, options).await?;
    }
    Command::Rollback { json } => status::run(&home, target, true, json).await?,
    Command::Check { channel, json } => {
      require_local(target)?;
      check(&home, channel, json).await?;
    }
    Command::Release(options) => {
      require_local(target)?;
      release::run(&home, options).await?;
    }
    Command::Prune { json } => {
      require_local(target)?;
      let result = tokio::task::spawn_blocking(move || {
        let installations = ctl_core::installations::Installations::new(&home).prune()?;
        let cache = ctl_client::installation::prune_caches(&home)?;
        Ok::<_, io::Error>(serde_json::json!({ "installations": installations, "cache": cache }))
      })
      .await
      .map_err(io::Error::other)??;
      if json {
        println!(
          "{}",
          serde_json::to_string(&result).map_err(io::Error::other)?
        );
      } else {
        println!("Removed unused installations; active, previous and running builds were kept.");
      }
    }
    Command::InstallArchive => {
      require_local(target)?;
      archive::install(&home).await?;
    }
    Command::ActivateInstalled { bundle_id } => {
      require_local(target)?;
      return archive::activate(&home, &bundle_id).await;
    }
  }
  Ok(0)
}

#[cfg(not(unix))]
fn run(_arguments: Arguments) -> std::future::Ready<io::Result<i32>> {
  std::future::ready(Err(io::Error::other(
    "ctlup installation management currently requires macOS or Linux",
  )))
}

#[cfg(unix)]
fn require_local(target: TargetOptions<'_>) -> io::Result<()> {
  if target.host.is_some() || target.destination.is_some() || target.method.is_some() {
    return Err(io::Error::other(
      "this ctlup operation is local; omit host options",
    ));
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

#[cfg(unix)]
fn selected_channel(
  home: &std::path::Path,
  channel: Option<crate::components::Channel>,
) -> io::Result<ctl_core::component_update::BundleChannel> {
  channel.map_or_else(
    || ctl_client::installation::default_channel(home),
    |channel| Ok(channel.into()),
  )
}

#[cfg(unix)]
async fn check(
  home: &std::path::Path,
  channel: Option<crate::components::Channel>,
  json: bool,
) -> io::Result<()> {
  let channel = selected_channel(home, channel)?;
  let catalog = published::check(channel).await.map_err(io::Error::other)?;
  if json {
    println!(
      "{}",
      serde_json::to_string(&catalog).map_err(io::Error::other)?
    );
  } else {
    println!(
      "{}: {} (revision {})",
      catalog.channel.name(),
      catalog.bundle_id,
      catalog.git_revision
    );
    print_targets(&catalog.targets);
  }
  Ok(())
}

#[cfg(unix)]
fn print_targets(targets: &[published::PublishedTarget]) {
  for target in targets {
    println!(
      "{}: {}",
      target.target_triple,
      if target.compatible {
        "compatible"
      } else {
        "incompatible"
      }
    );
  }
}
