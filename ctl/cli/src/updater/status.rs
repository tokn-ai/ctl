use super::{TargetOptions, show};
use ctl_client::installation::InstallationStatus;
use std::io;
use std::path::Path;

pub(super) async fn run(
  home: &Path,
  options: TargetOptions<'_>,
  rollback: bool,
  json: bool,
) -> io::Result<()> {
  if !rollback && !json {
    return show::run(home, options, false).await;
  }
  let is_remote =
    options.host.is_some() || options.destination.is_some() || options.method.is_some();
  let target = if is_remote {
    crate::bundled::register()?;
    Some(remote_target(options).await?)
  } else {
    None
  };
  let status = if let Some(target) = &target {
    remote_status(target, rollback).await?
  } else {
    if rollback {
      rollback_local(home).await?;
    }
    ctl_client::installation::status(home)?
  };
  if json {
    // Preserve the existing status/rollback JSON contract exactly.
    println!(
      "{}",
      serde_json::to_string(&status).map_err(io::Error::other)?
    );
  } else {
    let snapshot = if let Some(target) = &target {
      show::remote_snapshot(target, Some(status)).await?
    } else {
      ctl_client::installation::show(home)?
    };
    show::print(&snapshot, false, !is_remote && in_checkout())?;
    if rollback {
      println!(
        "Previous installation restored. Running services were preserved; restart separately."
      );
    }
  }
  Ok(())
}

async fn rollback_local(home: &Path) -> io::Result<()> {
  let account = home.to_owned();
  tokio::task::spawn_blocking(move || {
    let installs = ctl_core::installations::Installations::new(&account);
    installs.rollback()?;
    if let Err(error) = installs.prune() {
      eprintln!("ctlup: rollback completed; cleanup deferred: {error}");
    }
    Ok(())
  })
  .await
  .map_err(io::Error::other)?
}

pub(super) async fn remote_target(options: TargetOptions<'_>) -> io::Result<super::remote::Target> {
  let mut resolved = crate::target::resolve(options.host, options.destination, options.method)
    .await
    .map_err(io::Error::other)?;
  crate::target::apply_remote_platform(&mut resolved.target, options.platform);
  let target = super::remote::Target::connect(resolved.target).await?;
  if target.expected_account().is_none() {
    target.verify_account().await?;
  }
  Ok(target)
}

async fn remote_status(
  target: &super::remote::Target,
  rollback: bool,
) -> io::Result<InstallationStatus> {
  let expected = target.expected_account();
  let status = if rollback {
    ctl_client::installation::remote_rollback(
      &target.destination,
      &target.options,
      &target.interaction,
      expected,
    )
    .await
  } else {
    ctl_client::installation::remote_status(
      &target.destination,
      &target.options,
      &target.interaction,
      expected,
    )
    .await
  }
  .map_err(io::Error::other)?;
  if rollback && expected.is_none() {
    target.verify_account().await?;
  }
  Ok(status)
}

pub(super) fn in_checkout() -> bool {
  std::env::current_dir().is_ok_and(|directory| {
    directory.ancestors().any(|root| {
      root.join(".git").exists()
        && root.join("Cargo.toml").is_file()
        && root.join("ctl/cli/Cargo.toml").is_file()
        && root.join("scripts/dev/ctld-signed.mts").is_file()
    })
  })
}
