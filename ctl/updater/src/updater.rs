//! Installation-manager frontend; runtime commands remain in ctl and ctmux.

use clap::Parser;
use ctl_client::cli::Channel;
use ctl_client::component_update::BundleChannel;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod app;
mod archive;
mod arguments;
mod component;
mod ctream;
mod development;
#[cfg(any(unix, windows))]
mod launcher;
mod release;
mod remote;
mod show;
mod sign;
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
  #[cfg(any(unix, windows))]
  match launcher::dispatch() {
    Ok(Some(code)) => std::process::exit(code),
    Ok(None) => {}
    Err(error) => {
      eprintln!("ctlup: {error}");
      std::process::exit(1);
    }
  }
  if let Some(code) = ctl_client::cli::openssh::run_transport_helper(crate::bundled::register) {
    std::process::exit(code);
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
  #[cfg(any(unix, windows))]
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
  #[cfg(any(unix, windows))]
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
  match arguments.command {
    Command::ComponentInfo | Command::InstallationLayoutVersion => unreachable!(),
    Command::Install(options) => run_install(&home, &options).await?,
    Command::Update(options) => run_update(&home, &options).await?,
    Command::Bundle { command } => component::run(&home, command).await?,
    Command::Ctream { command } => {
      return ctream::run(&home, command).await;
    }
    Command::CatalogJson => {
      print_catalog(&home)?;
    }
    Command::PrepareInstall => {
      tokio::task::spawn_blocking(move || {
        ctl_core::installations::Installations::new(&home).prepare_installation()
      })
      .await
      .map_err(io::Error::other)??;
    }
    Command::Status(options) => run_status(&home, &options).await?,
    Command::InspectRuntime { json } => {
      if !json {
        return Err(io::Error::other("inspect-runtime requires --json"));
      }
      print_json(&ctl_client::installation::status(&home)?)?;
    }
    Command::RuntimeSource => {
      print_json(&ctl_client::installation::runtime_source(&home)?)?;
    }
    Command::InstallManager => {
      ctl_client::installation::provision_manager_from(&home, &std::env::current_exe()?).await?;
    }
    Command::ExportManager { sha256 } => {
      return tokio::task::spawn_blocking(move || {
        use std::io::Write as _;

        let Some(bytes) =
          ctl_core::installations::Installations::new(&home).manager_bytes(&sha256)?
        else {
          return Ok(10);
        };
        let mut output = io::stdout().lock();
        output.write_all(&bytes)?;
        output.flush()?;
        Ok(0)
      })
      .await
      .map_err(io::Error::other)?;
    }
    Command::Dev(options) => development::run(&home, options).await?,
    Command::Sign(options) => sign::run(&home, options).await?,
    Command::Release(options) => {
      release::run(&home, options).await?;
    }
    Command::Prune {
      forget_missing,
      json,
    } => {
      prune(home, forget_missing, json).await?;
    }
    Command::InstallArchive { origin_hex } => {
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
      let origin = origin_hex
        .as_deref()
        .map(ctl_client::installation::decode_catalog_origin)
        .transpose()?;
      return archive::activate(&home, &bundle_id, origin).await;
    }
    Command::InstallPublication(request) => {
      return archive::install_publication(&home, request).await;
    }
    Command::UpdaterDigest => {
      print_updater_digest(&std::env::current_exe()?)?;
    }
  }
  Ok(0)
}

async fn run_install(home: &Path, options: &arguments::InstallArguments) -> io::Result<()> {
  let targets = options.destination.resolve()?;
  if options.package == arguments::PackageSelection::Runtime {
    return update::install(home, options, &targets).await;
  }
  require_local(targets[0])?;
  if options.local_build || options.ctld_package.is_some() {
    return Err(io::Error::other(
      "native runtime file options require the runtime package",
    ));
  }
  #[cfg(any(target_os = "macos", target_os = "linux", windows))]
  {
    app::install(
      home,
      options.channel,
      options.release.as_deref(),
      options.pr,
      options.from.as_deref(),
      options.json,
    )
    .await
  }
  #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
  Err(unsupported_app())
}

async fn run_update(home: &Path, options: &arguments::UpdateArguments) -> io::Result<()> {
  let targets = options.destination.resolve()?;
  if options.package == arguments::PackageSelection::Runtime {
    return update::run(home, &targets, options.check, options.json).await;
  }
  require_local(targets[0])?;
  #[cfg(any(target_os = "macos", target_os = "linux", windows))]
  {
    app::update(home, options.check, options.json).await
  }
  #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
  Err(unsupported_app())
}

async fn run_status(home: &Path, options: &arguments::StatusArguments) -> io::Result<()> {
  let targets = options.destination.resolve()?;
  if options.package == arguments::PackageSelection::Runtime {
    return status::run(home, &targets, options.json).await;
  }
  require_local(targets[0])?;
  #[cfg(any(target_os = "macos", target_os = "linux", windows))]
  {
    app::status(home, options.json)
  }
  #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
  Err(unsupported_app())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn unsupported_app() -> io::Error {
  io::Error::new(
    io::ErrorKind::Unsupported,
    "desktop installation requires macOS, Linux or Windows",
  )
}

fn print_catalog(home: &Path) -> io::Result<()> {
  let catalog = ctl_client::installation::InstallationCatalog::new(
    ctl_core::installations::Installations::new(home).catalog()?,
  );
  catalog.validate()?;
  print_json(&catalog)
}

fn print_json(value: &impl serde::Serialize) -> io::Result<()> {
  println!(
    "{}",
    serde_json::to_string(value).map_err(io::Error::other)?
  );
  Ok(())
}

fn print_updater_digest(executable: &Path) -> io::Result<()> {
  let file = std::fs::File::open(executable)?;
  let (_, digest) = ctl_core::bundles::sha256_reader(file)?;
  println!("{}", hex::encode(digest));
  Ok(())
}

async fn prune(home: PathBuf, forget_missing: bool, json: bool) -> io::Result<()> {
  let (installations, cache) = tokio::task::spawn_blocking(move || {
    let store = ctl_core::installations::Installations::new(&home);
    let collect = || store.prune_all(forget_missing);
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    let installations = app::with_current_launch_entry(&home, collect)?;
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let installations = collect()?;
    let cache = ctl_client::installation::prune_caches(&home).map_err(|error| {
      io::Error::new(
        error.kind(),
        format!("installation cleanup completed, but cache cleanup failed: {error}"),
      )
    })?;
    Ok::<_, io::Error>((installations, cache))
  })
  .await
  .map_err(io::Error::other)??;
  if json {
    print_json(&serde_json::json!({ "installations": installations, "cache": cache }))?;
  } else {
    println!(
      "Removed {} unused runtime builds.",
      installations.removed.len()
    );
    if !installations.missing_builds.is_empty() {
      println!(
        "Kept {} missing build records for repair.",
        installations.missing_builds.len()
      );
    }
    if !installations.forgotten_missing_builds.is_empty() {
      println!(
        "Removed {} missing builds from the catalog.",
        installations.forgotten_missing_builds.len()
      );
    }
  }
  Ok(())
}

impl arguments::DestinationArguments {
  fn resolve(&self) -> io::Result<Vec<TargetOptions<'_>>> {
    target_options(
      &self.host,
      self.ssh.as_deref(),
      self.method.as_deref(),
      self.remote_platform,
    )
  }
}

fn target_options<'a>(
  hosts: &'a [String],
  ssh: Option<&'a str>,
  method: Option<&'a str>,
  platform: Option<crate::RemotePlatform>,
) -> io::Result<Vec<TargetOptions<'a>>> {
  if platform.is_some() && ssh.is_none() && hosts.is_empty() {
    return Err(io::Error::other(
      "--remote-platform requires --host or --ssh",
    ));
  }
  if hosts.len() > 1 && (method.is_some() || platform.is_some()) {
    return Err(io::Error::other(
      "--method and --remote-platform require a single destination",
    ));
  }
  let mut targets = Vec::new();
  if hosts.is_empty() {
    targets.push(TargetOptions {
      host: None,
      destination: ssh,
      method: None,
      platform,
    });
  } else {
    for host in hosts {
      if targets
        .iter()
        .any(|target| target.host == Some(host.as_str()))
      {
        continue;
      }
      targets.push(TargetOptions {
        host: Some(host),
        destination: None,
        method,
        platform,
      });
    }
  }
  Ok(targets)
}

impl TargetOptions<'_> {
  fn label(&self) -> &str {
    self.host.or(self.destination).unwrap_or("local")
  }

  fn remote(&self) -> bool {
    self.host.is_some() || self.destination.is_some()
  }

  async fn resolve(&self) -> io::Result<ctl_client::hosts::ConnectionTargetDto> {
    let mut resolved = crate::target::resolve(self.host, self.destination, self.method)
      .await
      .map_err(target_error)?;
    crate::target::apply_remote_platform(&mut resolved.target, self.platform);
    Ok(resolved.target)
  }
}

fn target_error(mut error: ctl_client::hosts::HostError) -> io::Error {
  const OLD_HINT: &str = "use --destination for an SSH address or config alias.";
  match error.code.as_str() {
    "host_not_found" => {
      if let Some(prefix) = error.message.strip_suffix(OLD_HINT) {
        error.message = format!("{prefix}use --ssh for an SSH address or config alias.");
      }
    }
    "target_conflict" => {
      error.message = "--ssh cannot be combined with --host or --method.".into();
    }
    "invalid_ssh_target"
      if error.message == "Use a valid SSH destination or config alias with --destination." =>
    {
      error.message = "Use a valid SSH address or config alias with --ssh.".into();
    }
    _ => {}
  }
  io::Error::other(error)
}

fn require_local(target: TargetOptions<'_>) -> io::Result<()> {
  if target.remote() {
    return Err(io::Error::other(
      "this ctlup operation is local; omit --host and --ssh",
    ));
  }
  Ok(())
}

fn tracked_channel(
  origin: Option<&ctl_core::installations::CatalogOrigin>,
) -> Option<BundleChannel> {
  match origin?.channel.as_str() {
    "stable" => Some(BundleChannel::Stable),
    "nightly" => Some(BundleChannel::Nightly),
    _ => None,
  }
}

fn selected_channel(home: &Path, channel: Option<Channel>) -> io::Result<BundleChannel> {
  channel.map_or_else(
    || ctl_client::installation::default_channel(home),
    |channel| Ok(channel.into()),
  )
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
