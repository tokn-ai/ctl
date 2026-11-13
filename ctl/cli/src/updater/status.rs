use super::TargetOptions;
use ctl_client::installation::{InstallationCatalog, InstallationStatus};
use ctl_core::installations::CatalogEntry;
use std::collections::BTreeMap;
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
  let (status, catalog) = if is_remote {
    crate::bundled::register()?;
    remote(options, rollback, !json).await?
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
    let status = ctl_client::installation::status(home)?;
    let catalog = if json {
      None
    } else {
      Some(InstallationCatalog::new(
        ctl_core::installations::Installations::new(home).catalog()?,
      ))
    };
    (status, catalog)
  };
  if json {
    println!(
      "{}",
      serde_json::to_string(&status).map_err(io::Error::other)?
    );
  } else {
    println!(
      "{}",
      render(&status, catalog.as_ref(), !is_remote && in_checkout())
    );
    if rollback {
      println!(
        "Previous installation restored. Running services were preserved; restart separately."
      );
    }
  }
  Ok(())
}

async fn remote_target(options: TargetOptions<'_>) -> io::Result<super::remote::Target> {
  let mut resolved = crate::target::resolve(options.host, options.destination, options.method)
    .await
    .map_err(io::Error::other)?;
  crate::target::apply_remote_platform(&mut resolved.target, options.platform);
  let target = super::remote::Target::connect(resolved.target).await?;
  let expected_account = target.expected_account();
  if expected_account.is_none() {
    target.verify_account().await?;
  }
  Ok(target)
}

/// Reads the optional installed catalog through the same account verification
/// used by installation status. Older remote managers have no catalog metadata.
pub(super) async fn catalog(
  home: &Path,
  options: TargetOptions<'_>,
) -> io::Result<Option<InstallationCatalog>> {
  if options.host.is_none() && options.destination.is_none() && options.method.is_none() {
    return Ok(Some(InstallationCatalog::new(
      ctl_core::installations::Installations::new(home).catalog()?,
    )));
  }
  crate::bundled::register()?;
  let target = remote_target(options).await?;
  ctl_client::installation::remote_catalog(
    &target.destination,
    &target.options,
    &target.interaction,
    target.expected_account(),
  )
  .await
  .map_err(io::Error::other)
}

async fn remote(
  options: TargetOptions<'_>,
  rollback: bool,
  include_catalog: bool,
) -> io::Result<(InstallationStatus, Option<InstallationCatalog>)> {
  let target = remote_target(options).await?;
  let expected_account = target.expected_account();
  let status = if rollback {
    ctl_client::installation::remote_rollback(
      &target.destination,
      &target.options,
      &target.interaction,
      expected_account,
    )
    .await
  } else {
    ctl_client::installation::remote_status(
      &target.destination,
      &target.options,
      &target.interaction,
      expected_account,
    )
    .await
  }
  .map_err(io::Error::other)?;
  if rollback && expected_account.is_none() {
    target.verify_account().await?;
  }
  let catalog = if include_catalog {
    ctl_client::installation::remote_catalog(
      &target.destination,
      &target.options,
      &target.interaction,
      expected_account,
    )
    .await
    .map_err(io::Error::other)?
  } else {
    None
  };
  Ok((status, catalog))
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

fn render(
  status: &InstallationStatus,
  catalog: Option<&InstallationCatalog>,
  development: bool,
) -> String {
  let mut lines = Vec::new();
  if status.active.is_none()
    && status.previous.is_none()
    && catalog.is_none_or(|catalog| catalog.installations.is_empty())
  {
    lines.push("No managed ctl installation.".into());
    if development {
      lines.push("Run `ctlup dev` to build and install this checkout.".into());
    } else {
      lines.push("Run `ctlup install --help` for installation options.".into());
    }
  } else {
    for (label, manifest) in [("Active", &status.active), ("Previous", &status.previous)] {
      if let Some(manifest) = manifest {
        let version = &manifest.components["ctl-agent"].build.version;
        let built_at = catalog
          .and_then(|catalog| {
            catalog
              .installations
              .iter()
              .filter(|entry| entry.bundle_id == manifest.bundle_id)
              .find_map(|entry| entry.origin.as_ref()?.built_at.as_deref())
          })
          .unwrap_or("unknown");
        lines.push(format!(
          "{label}: {} ({}, {})\n  Build date {}",
          crate::table::text(version),
          manifest.target_triple,
          manifest.bundle_id,
          crate::table::text(built_at)
        ));
      } else {
        lines.push(format!("{label}: none"));
      }
    }
  }
  if let Some(catalog) = catalog {
    if !catalog.installations.is_empty() {
      lines.push(render_catalog(catalog));
    }
  } else {
    lines.push("Installed bundle catalog unavailable.".into());
  }
  lines.join("\n")
}

pub(super) fn print_catalog(catalog: &InstallationCatalog) {
  println!("{}", render_catalog(catalog));
}

fn render_catalog(catalog: &InstallationCatalog) -> String {
  if catalog.installations.is_empty() {
    return "No installed component bundles.".into();
  }
  let mut groups = BTreeMap::<(&str, &str), Vec<&CatalogEntry>>::new();
  for entry in &catalog.installations {
    let channel = entry
      .origin
      .as_ref()
      .map_or("unknown", |origin| origin.channel.as_str());
    groups
      .entry((channel, &entry.target_triple))
      .or_default()
      .push(entry);
  }
  let mut lines = vec!["Installed component bundles".into()];
  for ((channel, target), mut entries) in groups {
    lines.push(format!("  {} / {}", crate::table::text(channel), target));
    entries.sort_by_key(|entry| (!entry.current, entry.bundle_id.as_str()));
    for entry in entries {
      let built_at = entry
        .origin
        .as_ref()
        .and_then(|origin| origin.built_at.as_deref())
        .unwrap_or("unknown");
      let state = if entry.current {
        " (current)"
      } else if entry.origin.is_some() {
        " (retained)"
      } else {
        ""
      };
      lines.push(format!(
        "    {}{state}  build date {}",
        entry.bundle_id,
        crate::table::text(built_at)
      ));
    }
  }
  lines.join("\n")
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctl_core::bundles::{Manifest, Source};
  use ctl_core::component::{ComponentBuildInfo, ComponentInfo};
  use ctl_core::installations::CatalogOrigin;

  fn manifest(id: char) -> Manifest {
    Manifest {
      schema_version: 1,
      bundle_id: id.to_string().repeat(64),
      target_triple: "x86_64-unknown-linux-musl".into(),
      source: Source::Ci,
      distribution_id: None,
      components: BTreeMap::from([(
        "ctl-agent".into(),
        ComponentInfo {
          build: ComponentBuildInfo {
            version: "0.1.3".into(),
            source_revision: Some(id.to_string().repeat(40)),
            source_fingerprint: id.to_string().repeat(64),
            dirty: false,
          },
          protocols: Vec::new(),
        },
      )]),
      files: BTreeMap::new(),
    }
  }

  #[test]
  fn status_uses_recorded_build_dates_and_keeps_legacy_dates_unknown() {
    let active = manifest('a');
    let previous = manifest('b');
    let catalog = InstallationCatalog::new(vec![
      CatalogEntry {
        bundle_id: active.bundle_id.clone(),
        target_triple: active.target_triple.clone(),
        origin: Some(CatalogOrigin {
          channel: "nightly".into(),
          publication_id: None,
          built_at: Some("2026-11-13T10:15:00Z".into()),
        }),
        current: true,
      },
      CatalogEntry {
        bundle_id: previous.bundle_id.clone(),
        target_triple: previous.target_triple.clone(),
        origin: None,
        current: false,
      },
    ]);
    let status = InstallationStatus {
      active: Some(active),
      previous: Some(previous),
    };
    let text = render(&status, Some(&catalog), false);
    assert!(text.contains("Build date 2026-11-13T10:15:00Z"));
    assert!(text.contains("Build date unknown"));
    assert!(text.contains("nightly / x86_64-unknown-linux-musl"));
    assert!(text.contains("unknown / x86_64-unknown-linux-musl"));
    let legacy = render(&status, None, false);
    assert_eq!(legacy.matches("Build date unknown").count(), 2);
    assert!(legacy.contains("Installed bundle catalog unavailable."));
  }

  #[test]
  fn catalog_groups_channels_and_targets_with_current_before_retained() {
    let entry = |channel: &str, target: &str, id: char, current| CatalogEntry {
      bundle_id: id.to_string().repeat(64),
      target_triple: target.into(),
      origin: Some(CatalogOrigin {
        channel: channel.into(),
        publication_id: None,
        built_at: None,
      }),
      current,
    };
    let older = entry("nightly", "aarch64-apple-darwin", 'a', false);
    let current = entry("nightly", "aarch64-apple-darwin", 'b', true);
    let catalog = InstallationCatalog::new(vec![
      entry("stable", "x86_64-pc-windows-msvc", 'c', true),
      older.clone(),
      current.clone(),
    ]);
    let text = render_catalog(&catalog);
    assert!(text.contains("nightly / aarch64-apple-darwin"));
    assert!(text.contains("stable / x86_64-pc-windows-msvc"));
    assert!(text.find(&current.bundle_id).unwrap() < text.find(&older.bundle_id).unwrap());
    assert!(text.contains("(current)  build date unknown"));
    assert!(text.contains("(retained)  build date unknown"));
    let status = InstallationStatus {
      active: None,
      previous: None,
    };
    let text = render(&status, Some(&catalog), false);
    assert!(text.contains("Active: none"));
    assert!(!text.contains("No managed ctl installation."));
  }
}
