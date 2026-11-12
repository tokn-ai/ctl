use super::TargetOptions;
use ctl_client::hosts::ConnectionTargetDto;
use ctl_client::installation::InstallationStatus;
use std::io;
use std::path::Path;

pub(super) async fn run(
  home: &Path,
  options: TargetOptions<'_>,
  rollback: bool,
  json: bool,
) -> io::Result<()> {
  let is_remote =
    options.host.is_some() || options.destination.is_some() || options.method.is_some();
  let status = if is_remote {
    crate::bundled::register()?;
    remote(options, rollback).await?
  } else {
    if rollback {
      let account = home.to_owned();
      tokio::task::spawn_blocking(move || {
        let installs = ctl_core::installations::Installations::new(&account);
        installs.rollback()?;
        if let Err(error) = installs.prune() {
          eprintln!("ctlup: rollback completed; cleanup deferred: {error}");
        }
        Ok::<_, io::Error>(())
      })
      .await
      .map_err(io::Error::other)??;
    }
    ctl_client::installation::status(home)?
  };
  if json {
    println!(
      "{}",
      serde_json::to_string(&status).map_err(io::Error::other)?
    );
  } else {
    print(&status, !is_remote && in_checkout());
    if rollback {
      println!(
        "Previous installation restored. Running services were preserved; restart separately."
      );
    }
  }
  Ok(())
}

async fn remote(options: TargetOptions<'_>, rollback: bool) -> io::Result<InstallationStatus> {
  let resolved = crate::target::resolve(options.host, options.destination, options.method)
    .await
    .map_err(io::Error::other)?;
  let target = resolved.target;
  if target.remote_platform() == ctl_client::RemotePlatform::Windows {
    return Err(io::Error::other(
      "remote installation management requires a Unix SSH host",
    ));
  }
  crate::target::ensure_vpn(&target)
    .await
    .map_err(io::Error::other)?;
  let control_path =
    crate::ssh_broker::ensure_master(target.to_ssh_target().map_err(io::Error::other)?)
      .await
      .map_err(io::Error::other)?;
  let ctl_client::ConnectionTarget::Ssh {
    destination,
    options,
  } = target.to_core()
  else {
    return Err(io::Error::other("the selected host is local; omit --host"));
  };
  verify_account(&target, &destination, &options, &control_path).await?;
  let interaction = ctl_client::SshInteraction::Multiplexed {
    control_path: control_path.clone(),
  };
  let status = if rollback {
    ctl_client::installation::remote_rollback(&destination, &options, &interaction).await
  } else {
    ctl_client::installation::remote_status(&destination, &options, &interaction).await
  }
  .map_err(io::Error::other)?;
  if rollback {
    verify_account(&target, &destination, &options, &control_path).await?;
  }
  Ok(status)
}

async fn verify_account(
  target: &ConnectionTargetDto,
  destination: &str,
  options: &ctl_client::SshConnectionOptions,
  control_path: &Path,
) -> io::Result<()> {
  if matches!(
    target,
    ConnectionTargetDto::Ssh {
      remote_info: Some(_),
      ..
    }
  ) {
    let id = ctl_client::maintenance::inspect_remote_id(destination, options, control_path)
      .await
      .map_err(io::Error::other)?;
    target.verify_remote_id(&id).map_err(io::Error::other)?;
  }
  Ok(())
}

fn in_checkout() -> bool {
  std::env::current_dir().is_ok_and(|directory| {
    directory.ancestors().any(|root| {
      root.join(".git").exists()
        && root.join("Cargo.toml").is_file()
        && root.join("ctl/cli/Cargo.toml").is_file()
        && root.join("scripts/dev/ctld-signed.mts").is_file()
    })
  })
}

fn print(status: &InstallationStatus, development: bool) {
  if status.active.is_none() && status.previous.is_none() {
    println!("No managed ctl installation.");
    if development {
      println!("Run `ctlup dev` to build and install this checkout.");
    } else {
      println!("Run `ctlup install --help` for installation options.");
    }
    return;
  }
  for (label, manifest) in [("Active", &status.active), ("Previous", &status.previous)] {
    if let Some(manifest) = manifest {
      let version = &manifest.components["ctl-agent"].build.version;
      println!(
        "{label}: {} ({}, {})",
        crate::table::text(version),
        manifest.target_triple,
        manifest.bundle_id
      );
    } else {
      println!("{label}: none");
    }
  }
}
