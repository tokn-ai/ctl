//! Complete target bundles are durable channel components, not executable overlays.

use super::arguments::ComponentCommand;
use super::{TargetOptions, require_local, show};
use ctl_client::component_update::{self, BuildSource, BundleChannel};
use ctl_client::installation;
use ctl_client::remote_bundle::{pr, published};
use ctl_core::installations::{CatalogOrigin, Installations};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

mod list;

pub(super) async fn run(
  home: &Path,
  target: TargetOptions<'_>,
  command: ComponentCommand,
) -> io::Result<()> {
  match command {
    ComponentCommand::List {
      channel,
      available,
      installed,
      json,
    } => {
      if available || !installed {
        require_local(target)?;
        return available_targets(home, channel.as_deref(), json).await;
      }
      if let Some(channel) = &channel {
        validate_channel(channel)?;
      }
      let installations = show::snapshot(home, target).await?.installations;
      print_list(&list::installed(&installations, channel.as_deref()), json)?;
    }
    ComponentCommand::Add {
      targets,
      channel,
      pr,
      from,
      json,
    } => {
      require_local(target)?;
      add(home, targets, channel.map(Into::into), pr, from, json).await?;
    }
    ComponentCommand::Remove {
      targets,
      channel,
      json,
    } => {
      require_local(target)?;
      let channel = match channel {
        Some(channel) => channel,
        None => installation::default_channel(home)?.name().into(),
      };
      validate_channel(&channel)?;
      let targets = validated_targets(targets)?;
      let account = home.to_owned();
      let removed = tokio::task::spawn_blocking(move || {
        let installs = Installations::new(&account);
        let mut removed = Vec::new();
        for target in targets {
          if installs.remove_component(&channel, &target)? {
            removed.push(target);
          }
        }
        if let Err(error) = installs.prune() {
          eprintln!("ctlup: registrations removed; installation cleanup deferred: {error}");
        }
        if let Err(error) = installation::prune_caches(&account) {
          eprintln!("ctlup: registrations removed; cache cleanup deferred: {error}");
        }
        Ok::<_, io::Error>(removed)
      })
      .await
      .map_err(io::Error::other)??;
      if json {
        print_json(&removed)?;
      } else if removed.is_empty() {
        println!("No matching installed components.");
      } else {
        println!(
          "Removed target registrations: {}. Running services were preserved.",
          removed.join(", ")
        );
      }
    }
  }
  Ok(())
}

async fn add(
  home: &Path,
  targets: Vec<String>,
  channel: Option<BundleChannel>,
  pull_request: Option<u64>,
  from: Option<PathBuf>,
  json: bool,
) -> io::Result<()> {
  let targets = validated_targets(targets)?;
  let snapshot = component_update::PreparationSnapshot::capture(home)?;
  let channel = match channel {
    Some(channel) => channel,
    None if pull_request.is_none() && from.is_none() => installation::default_channel(home)?,
    None => BundleChannel::default(),
  };
  let mut prepared = Vec::new();
  // Retain extracted PR files until the cohort has finished preparation.
  let mut pull_request_files = Vec::new();
  for target in targets {
    let source = if let Some(number) = pull_request {
      eprintln!("ctlup: Resolving PR #{number} for {target}…");
      let files = pr::download_for_target(number, &target)
        .await
        .map_err(io::Error::other)?;
      let source = BuildSource::Provided {
        path: files.directory().into(),
        local_build: false,
        ctld_package: None,
      };
      pull_request_files.push(files);
      source
    } else if let Some(path) = &from {
      BuildSource::Provided {
        path: path.clone(),
        local_build: false,
        ctld_package: None,
      }
    } else {
      BuildSource::Published { channel }
    };
    let operation = component_update::prepare_component(home, &target, &source, &[], |progress| {
      eprintln!("ctlup: {progress}");
    });
    let mut component = tokio::select! {
      result = operation => result?,
      _ = tokio::signal::ctrl_c() => return Err(io::Error::new(io::ErrorKind::Interrupted, "Component preparation cancelled; installed targets were preserved.")),
    };
    component.retain_earlier_snapshot(&snapshot);
    prepared.push(component);
  }
  let origin = pull_request.map(|number| {
    let mut origin = prepared[0].origin.clone().unwrap_or(CatalogOrigin {
      channel: "provided".into(),
      publication_id: prepared[0].bundle.manifest.distribution_id.clone(),
      built_at: None,
    });
    origin.channel = format!("pr-{number}");
    origin
  });
  // Installation commits the complete target set only after all preparation.
  let result = component_update::install_prepared_components(home, prepared, origin, false).await?;
  if json {
    print_json(&result)?;
  } else {
    for installed in result {
      println!(
        "Installed {} ({})",
        installed.target_triple, installed.bundle_id
      );
    }
    println!("Run `ctlup component list` to inspect installed components.");
  }
  drop(pull_request_files);
  Ok(())
}

async fn available_targets(home: &Path, channel: Option<&str>, json: bool) -> io::Result<()> {
  let channel = match channel {
    Some("stable") => BundleChannel::Stable,
    Some("nightly") => BundleChannel::Nightly,
    Some(_) => {
      return Err(io::Error::other(
        "available targets require the stable or nightly publication channel",
      ));
    }
    None => installation::default_channel(home)?,
  };
  let catalog = published::check(channel).await.map_err(io::Error::other)?;
  let installed = installation::show(home)?.installations;
  print_list(&list::available(catalog.targets, &installed), json)
}

fn print_list(components: &list::ComponentList, json: bool) -> io::Result<()> {
  if json {
    print_json(components)
  } else {
    println!("{}", components.render());
    Ok(())
  }
}

fn validated_targets(targets: Vec<String>) -> io::Result<BTreeSet<String>> {
  if targets.is_empty() || targets.len() > 16 {
    return Err(io::Error::other(
      "choose between one and sixteen complete target bundles",
    ));
  }
  targets
    .into_iter()
    .map(|name| {
      let target = name.strip_prefix("bundle-").unwrap_or(&name);
      ctl_core::bundles::validate_target(target)?;
      Ok(target.to_owned())
    })
    .collect()
}

fn validate_channel(channel: &str) -> io::Result<()> {
  CatalogOrigin {
    channel: channel.into(),
    publication_id: None,
    built_at: None,
  }
  .validate()
}

fn print_json(value: &impl serde::Serialize) -> io::Result<()> {
  println!(
    "{}",
    serde_json::to_string(value).map_err(io::Error::other)?
  );
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn selectors_validate_before_preparing_or_removing_targets() {
    assert!(validated_targets(Vec::new()).is_err());
    assert!(validated_targets(vec!["../outside".into()]).is_err());
    assert!(validated_targets(vec!["bundle-../outside".into()]).is_err());
    assert!(validated_targets(vec!["bundle-".into()]).is_err());
    assert_eq!(
      validated_targets(vec!["x86_64-unknown-linux-musl".into(); 2])
        .unwrap()
        .len(),
      1
    );
    assert_eq!(
      validated_targets(vec![
        "bundle-aarch64-apple-darwin".into(),
        "aarch64-apple-darwin".into(),
        "bundle-x86_64-unknown-linux-musl".into(),
      ])
      .unwrap(),
      BTreeSet::from([
        "aarch64-apple-darwin".into(),
        "x86_64-unknown-linux-musl".into(),
      ])
    );
    for channel in ["stable", "nightly", "local", "provided", "pr-123"] {
      validate_channel(channel).unwrap();
    }
    assert!(validate_channel("../outside").is_err());
  }
}
