//! Installation-manager frontend; runtime commands remain in ctl and ctmux.

use clap::Parser;
use std::io;
use std::path::PathBuf;

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod app;
mod archive;
mod arguments;
mod component;
mod development;
#[cfg(windows)]
mod launcher;
mod release;
mod remote;
mod show;
mod status;
pub(crate) mod update;

use arguments::{Arguments, Command};
use ctl_client::remote_bundle::published;

#[derive(Clone, Copy)]
pub(crate) struct TargetOptions<'a> {
  host: Option<&'a str>,
  destination: Option<&'a str>,
  method: Option<&'a str>,
  platform: Option<crate::RemotePlatform>,
}

pub(crate) fn main() {
  #[cfg(windows)]
  match launcher::dispatch() {
    Ok(Some(code)) => std::process::exit(code),
    Ok(None) => {}
    Err(error) => {
      eprintln!("ctlup: {error}");
      std::process::exit(1);
    }
  }
  let arguments = Arguments::parse();
  if matches!(arguments.command, Command::InstallationLayoutVersion) {
    println!("{}", ctl_core::installations::DEVELOPMENT_LAYOUT_VERSION);
    return;
  }
  if matches!(arguments.command, Command::ComponentInfo) {
    if let Err(error) = crate::component_info::print() {
      eprintln!("ctlup: {error}");
      std::process::exit(1);
    }
    return;
  }
  #[cfg(windows)]
  let installation = ctl_core::installations::lease_current_executable().unwrap_or_else(|error| {
    eprintln!("ctlup: could not retain its installation: {error}");
    std::process::exit(1);
  });
  let runtime = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .unwrap_or_else(|error| {
      eprintln!("ctlup: could not start runtime: {error}");
      std::process::exit(1);
    });
  let result = runtime.block_on(run(arguments));
  drop(runtime);
  #[cfg(windows)]
  drop(installation);
  let code = match result {
    Ok(code) => code,
    Err(error) => {
      eprintln!("ctlup: {error}");
      1
    }
  };
  std::process::exit(code);
}

async fn run(arguments: Arguments) -> io::Result<i32> {
  let home = dirs::home_dir().ok_or_else(|| io::Error::other("home directory is unavailable"))?;
  let target = TargetOptions {
    host: arguments.host.as_deref(),
    destination: arguments.destination.as_deref(),
    method: arguments.method.as_deref(),
    platform: arguments.remote_platform,
  };
  validate_target_options(target)?;
  match arguments.command {
    Command::ComponentInfo | Command::InstallationLayoutVersion => unreachable!(),
    Command::Install(options) | Command::Update(options) => {
      update::run(&home, options, target).await?;
    }
    Command::App { command } => {
      require_local(target)?;
      #[cfg(any(target_os = "macos", target_os = "linux", windows))]
      app::run(&home, command).await?;
      #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
      {
        let _ = command;
        return Err(io::Error::other(
          "ctlup app installation currently requires macOS, Linux or Windows",
        ));
      }
    }
    Command::Component { command } => component::run(&home, target, command).await?,
    Command::Show { json } => show::run(&home, target, json).await?,
    Command::CatalogJson => {
      require_local(target)?;
      let catalog = ctl_client::installation::InstallationCatalog::new(
        ctl_core::installations::Installations::new(&home).catalog()?,
      );
      catalog.validate()?;
      println!(
        "{}",
        serde_json::to_string(&catalog).map_err(io::Error::other)?
      );
    }
    Command::Status { json } => status::run(&home, target, json).await?,
    Command::Dev(options) => {
      require_local(target)?;
      development::run(&home, options).await?;
    }
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
      prune(home, json).await?;
    }
    Command::InstallArchive { origin_hex } => {
      require_local(target)?;
      let origin = origin_hex
        .as_deref()
        .map(ctl_client::installation::decode_catalog_origin)
        .transpose()?;
      archive::install(&home, origin).await?;
    }
    Command::ActivateInstalled {
      bundle_id,
      origin_hex,
    } => {
      require_local(target)?;
      let origin = origin_hex
        .as_deref()
        .map(ctl_client::installation::decode_catalog_origin)
        .transpose()?;
      return archive::activate(&home, &bundle_id, origin).await;
    }
    #[cfg(windows)]
    Command::UpdaterDigest => {
      require_local(target)?;
      updater_digest()?;
    }
  }
  Ok(0)
}

#[cfg(windows)]
fn updater_digest() -> io::Result<()> {
  use sha2::{Digest as _, Sha256};
  let executable = std::env::current_exe()?;
  let mut file = std::fs::File::open(executable)?;
  let mut digest = Sha256::new();
  io::copy(&mut file, &mut digest)?;
  println!("{:x}", digest.finalize());
  Ok(())
}

async fn prune(home: PathBuf, json: bool) -> io::Result<()> {
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
    println!(
      "Removed unused installations. Kept channel targets, checkout builds, the active build, and running builds."
    );
  }
  Ok(())
}

fn validate_target_options(target: TargetOptions<'_>) -> io::Result<()> {
  if target.destination.is_some() && (target.host.is_some() || target.method.is_some()) {
    return Err(io::Error::other(
      "--destination cannot be combined with --host or --method",
    ));
  }
  if target.platform.is_some() && target.host.is_none() && target.destination.is_none() {
    return Err(io::Error::other(
      "--remote-platform requires --host or --destination",
    ));
  }
  Ok(())
}

fn require_local(target: TargetOptions<'_>) -> io::Result<()> {
  if target.host.is_some() || target.destination.is_some() || target.method.is_some() {
    return Err(io::Error::other(
      "this ctlup operation is local; omit host options",
    ));
  }
  Ok(())
}

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

fn selected_channel(
  home: &std::path::Path,
  channel: Option<crate::components::Channel>,
) -> io::Result<ctl_core::component_update::BundleChannel> {
  channel.map_or_else(
    || ctl_client::installation::default_channel(home),
    |channel| Ok(channel.into()),
  )
}

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
    println!("Built {}", catalog.built_at.as_deref().unwrap_or("unknown"));
    print_targets(&catalog.targets);
  }
  Ok(())
}

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
