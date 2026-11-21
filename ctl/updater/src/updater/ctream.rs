//! Keep ctream tools and their agent assets together in a local generation.

use super::arguments::{CtreamCommand, CtreamInstallArguments, DevelopmentArguments};
use ctl_bundle::ctream as bundle;
use ctl_core::private_fs;
use std::{
  ffi::OsString,
  fs, io,
  path::{Path, PathBuf},
  process::Command,
};

mod development;

pub(super) async fn run(home: &Path, command: CtreamCommand) -> io::Result<i32> {
  let root = bundle::root_for_account(home);
  match command {
    CtreamCommand::Install(options) => install(root, options, false).await?,
    CtreamCommand::Update(options) => install(root, options, true).await?,
    CtreamCommand::Status { json } => report(&root, bundle::active(&root)?.as_ref(), json)?,
    CtreamCommand::Run { args } => return launch(&installed(&root)?.preview, args),
    CtreamCommand::Cli { args } => return launch(&installed(&root)?.cli, args),
  }
  Ok(0)
}

fn installed(root: &Path) -> io::Result<bundle::Installation> {
  bundle::active(root)?.ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::NotFound,
      "ctream is not installed. Run ctlup ctream install or ctlup dev --ctream first.",
    )
  })
}

pub(super) async fn install_development(
  home: &Path,
  workspace: &Path,
  options: &DevelopmentArguments,
) -> io::Result<()> {
  let native = bundle::native_target()?;
  let target = options.target.as_deref().unwrap_or(native);
  if target != native && target != ctl_core::paths::native_target() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      format!(
        "ctream preview requires the native target {native}. Use --agent-target to build a remote agent."
      ),
    ));
  }
  let build = development::build(
    workspace,
    target,
    options.profile.as_deref(),
    options.release,
    options.target_dir.as_deref(),
    &options.ctream_agent_targets,
  );
  let files = tokio::select! {
    result = build => result?,
    _ = tokio::signal::ctrl_c() => return Err(io::Error::new(io::ErrorKind::Interrupted, "ctream development build cancelled.")),
  };
  let root = bundle::root_for_account(home);
  let install_root = root.clone();
  let result =
    tokio::task::spawn_blocking(move || bundle::install_development(&install_root, &files))
      .await
      .map_err(io::Error::other)??;
  report(&root, Some(&result), options.json)
}

async fn install(root: PathBuf, options: CtreamInstallArguments, update: bool) -> io::Result<()> {
  if update {
    installed(&root)?;
  }
  let target = bundle::native_target()?;
  let mut download = None;
  let archive = if let Some(path) = options.from {
    path
  } else {
    let staging = Download::new()?;
    let archive =
      ctl_client::remote_bundle::ctream::download(options.run, target, &staging.0).await?;
    download = Some(staging);
    archive
  };
  let install_root = root.clone();
  let result = tokio::task::spawn_blocking(move || {
    let _download = download;
    bundle::install(&install_root, &archive)
  })
  .await
  .map_err(io::Error::other)??;
  report(&root, Some(&result), options.json)
}

fn report(root: &Path, install: Option<&bundle::Installation>, json: bool) -> io::Result<()> {
  if json {
    let result = serde_json::json!({"root": root, "active": install});
    println!(
      "{}",
      serde_json::to_string(&result).map_err(io::Error::other)?
    );
  } else if let Some(install) = install {
    let source = if install.development {
      "development build"
    } else {
      "bundle"
    };
    println!("Active ctream {source} at {}", install.directory.display());
    println!("Open the preview with ctlup ctream run");
  } else {
    println!("No ctream installation under {}", root.display());
  }
  Ok(())
}

fn launch(path: &Path, args: Vec<OsString>) -> io::Result<i32> {
  let mut command = Command::new(path);
  command.args(args);
  ctl_client::cli::connection::run_process(command).map_err(io::Error::other)
}

struct Download(PathBuf);

impl Download {
  fn new() -> io::Result<Self> {
    let path = std::env::temp_dir().join(format!("ctlup-ctream-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path)?;
    Ok(Self(path))
  }
}

impl Drop for Download {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}
