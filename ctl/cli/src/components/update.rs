use clap::ValueEnum;
use ctl_client::component_update::{BuildSource, Package, UpdateResult};
use ctl_client::hosts::ConnectionTargetDto;
use ctl_core::bundles::Purpose;
use serde::Serialize;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum UpdatePackage {
  CtlAgent,
  FullBundle,
}
impl From<UpdatePackage> for Package {
  fn from(value: UpdatePackage) -> Self {
    match value {
      UpdatePackage::CtlAgent => Self::CtlAgent,
      UpdatePackage::FullBundle => Self::FullBundle,
    }
  }
}

#[derive(Serialize)]
struct HostResult {
  host: String,
  result: Option<UpdateResult>,
  error: Option<String>,
}

pub(super) struct TargetOptions<'a> {
  pub host: Option<&'a str>,
  pub method: Option<&'a str>,
  pub platform: Option<crate::RemotePlatform>,
}

pub(super) async fn run(
  home: &Path,
  mut hosts: Vec<String>,
  local: bool,
  options: ctl_client::component_update::UpdateOptions,
  target_options: TargetOptions<'_>,
  json: bool,
) -> io::Result<()> {
  let TargetOptions {
    host,
    method,
    platform,
  } = target_options;
  let ctl_client::component_update::UpdateOptions { package, source } = options;
  if method.is_some() && host.is_none() {
    return Err(io::Error::other(
      "--method requires --host; --hosts uses each host's preferred method",
    ));
  }
  if matches!(platform, Some(crate::RemotePlatform::Windows)) {
    return Err(io::Error::other(
      "component uploads currently require a Unix SSH host",
    ));
  }
  let choices = choices(&mut hosts, local, host);
  let package_label = match package {
    Package::CtlAgent => "ctl-agent",
    Package::FullBundle => "full bundle",
  };
  let mut results = Vec::new();
  for choice in choices {
    let label = choice.as_deref().unwrap_or("local").to_owned();
    eprintln!("ctl: Updating {label} ({package_label})…");
    let operation = update_host(
      home,
      choice.as_deref(),
      if choice.as_deref() == host {
        method
      } else {
        None
      },
      package,
      &source,
    );
    let outcome = tokio::select! {
      result = operation => result,
      _ = tokio::signal::ctrl_c() => return Err(io::Error::new(io::ErrorKind::Interrupted, "Update cancelled. Refresh component status before retrying; an activation may already have completed.")),
    };
    let (result, error) = match outcome {
      Ok(result) => {
        eprintln!("ctl: {label}: installed; running services preserved. Restart separately.");
        (Some(result), None)
      }
      Err(error) => {
        eprintln!("ctl: {label}: {error}");
        (None, Some(error.to_string()))
      }
    };
    results.push(HostResult {
      host: label,
      result,
      error,
    });
  }
  if json {
    println!(
      "{}",
      serde_json::to_string(&results).map_err(io::Error::other)?
    );
  }
  if results.iter().any(|result| result.error.is_some()) {
    return Err(io::Error::other(
      "some component updates failed; successful hosts were kept",
    ));
  }
  Ok(())
}

fn choices(hosts: &mut Vec<String>, local: bool, host: Option<&str>) -> Vec<Option<String>> {
  let mut choices = Vec::new();
  if let Some(host) = host {
    choices.push(Some(host.to_owned()));
  }
  for host in hosts.drain(..) {
    if !choices.contains(&Some(host.clone())) {
      choices.push(Some(host));
    }
  }
  if local || choices.is_empty() {
    choices.push(None);
  }
  choices
}

async fn update_host(
  home: &Path,
  host: Option<&str>,
  method: Option<&str>,
  package: Package,
  source: &BuildSource,
) -> io::Result<UpdateResult> {
  let resolved = crate::target::resolve(host, method)
    .await
    .map_err(io::Error::other)?;
  let target = resolved.target;
  if target.is_local() {
    let bundle = ctl_client::component_update::prepare(
      home,
      ctl_core::paths::native_target(),
      Purpose::Local,
      source,
      &crate::remote::update_bundle_directories()?,
    )
    .await?;
    return ctl_client::component_update::install_local(home, &bundle, package).await;
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
    unreachable!()
  };
  if matches!(
    target,
    ConnectionTargetDto::Ssh {
      remote_info: Some(_),
      ..
    }
  ) {
    let identity = ctl_client::maintenance::inspect_agent(&destination, &options, &control_path)
      .await
      .map_err(io::Error::other)?;
    target
      .verify_remote_identity(&identity)
      .map_err(io::Error::other)?;
  }
  let interaction = ctl_client::SshInteraction::Multiplexed {
    control_path: control_path.clone(),
  };
  let output = tokio::time::timeout(
    Duration::from_mins(1),
    ctl_client::probe_ssh_unix_platform_interactive(&destination, &options, &interaction),
  )
  .await
  .map_err(io::Error::other)?
  .map_err(io::Error::other)?;
  let platform =
    ctl_client::remote_bundle::Platform::parse_probe(&output).map_err(io::Error::other)?;
  let target_triple = platform.target_triple().map_err(io::Error::other)?;
  if matches!(source, BuildSource::Selected) {
    crate::remote::ensure_update_source(target_triple).await?;
  }
  let bundle =
    ctl_client::component_update::prepare(home, target_triple, Purpose::Upload, source, &[])
      .await?;
  let result =
    install_with_progress(&destination, &options, &interaction, &bundle, package).await?;
  if matches!(
    target,
    ConnectionTargetDto::Ssh {
      remote_info: Some(_),
      ..
    }
  ) {
    let identity = ctl_client::maintenance::inspect_agent(&destination, &options, &control_path)
      .await
      .map_err(io::Error::other)?;
    target
      .verify_remote_identity(&identity)
      .map_err(io::Error::other)?;
    verify_activation(&bundle, &identity, package)?;
  }
  Ok(result)
}

fn verify_activation(
  bundle: &ctl_core::bundles::Bundle,
  identity: &ctl_proto::RemoteIdentity,
  package: Package,
) -> io::Result<()> {
  let expected = bundle
    .manifest
    .distribution_id
    .as_ref()
    .unwrap_or(&bundle.manifest.bundle_id);
  let agent_matches =
    ctl_client::component_update::agent_matches(&bundle.manifest.components["ctl-agent"], identity);
  let activated = if package == Package::CtlAgent {
    agent_matches
  } else {
    identity.bundle.as_ref().is_some_and(|installed| {
      installed.bundle_id == *expected
        && installed.app_version == bundle.manifest.components["ctl-agent"].build.version
        && installed.target_triple == bundle.manifest.target_triple
        && installed.git_revision
          == bundle.manifest.components["ctl-agent"]
            .build
            .source_revision
            .as_deref()
            .unwrap_or_default()
    })
  };
  if !activated {
    return Err(io::Error::other(
      "Components installed, but activation could not be verified. Running sessions were preserved; refresh status before retrying.",
    ));
  }
  Ok(())
}

async fn install_with_progress(
  destination: &str,
  options: &ctl_client::SshConnectionOptions,
  interaction: &ctl_client::SshInteraction,
  bundle: &ctl_core::bundles::Bundle,
  package: Package,
) -> io::Result<UpdateResult> {
  let (updates, mut receiver) =
    tokio::sync::watch::channel(ctl_client::RemoteInstallProgress::default());
  let upload = ctl_client::component_update::install_remote(
    destination,
    options,
    interaction,
    bundle,
    package,
    |progress| {
      updates.send_replace(progress);
    },
  );
  tokio::pin!(upload);
  let mut tick = tokio::time::interval(Duration::from_millis(500));
  let mut watchdog = ctl_client::RemoteInstallWatchdog::new(Instant::now());
  let mut display = crate::remote::ProgressDisplay::default();
  let result = loop {
    tokio::select! {
      result = &mut upload => { display.finish(); break result.map_err(io::Error::other)?; },
      _ = tick.tick() => {},
      _ = receiver.changed() => {},
    }
    let progress = watchdog
      .observe(receiver.borrow().clone(), Instant::now(), false)
      .map_err(io::Error::other)?;
    display.show(&progress);
  };
  Ok(result)
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn default_local_and_explicit_batch_targets_are_distinct() {
    assert_eq!(choices(&mut vec![], false, None), vec![None]);
    assert_eq!(
      choices(
        &mut vec!["a".into(), "b".into(), "a".into()],
        true,
        Some("a")
      ),
      vec![Some("a".into()), Some("b".into()), None]
    );
  }
}
