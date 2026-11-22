use std::path::PathBuf;

use ctl_client::{
  ConnectionTarget, RemotePlatform, SshInteraction,
  component_update::{self, BuildSource},
  probe_ssh_unix_platform_interactive, probe_ssh_windows_platform,
  remote_bundle::{self, Platform},
};
#[cfg(test)]
use ctl_core::component::ComponentBuildInfo;
use tauri::{AppHandle, Manager as _, path::BaseDirectory};
use tokio::sync::watch;

#[cfg(unix)]
use crate::dto::RemoteAgentInstallResultDto;
use crate::dto::{ConnectionTargetDto, RemoteAgentInstallProgressDto as Progress};
use crate::error::{CommandErrorDto, CommandResult};
use crate::ssh_auth::install_identity;

mod progress;
pub(crate) use progress::preparation_progress;

pub(crate) struct Installation {
  #[cfg(unix)]
  pub version: RemoteAgentInstallResultDto,
  #[cfg(unix)]
  pub agent: ctl_core::component::ComponentInfo,
  pub result: ctl_core::component_update::UpdateResult,
}

pub async fn install(
  target: &ConnectionTargetDto,
  interaction: &SshInteraction,
  update: &ctl_client::component_update::UpdateOptions,
  included_bundles: &[PathBuf],
  on_progress: impl Fn(Progress) -> CommandResult<()>,
  authenticating: impl Fn() -> bool,
) -> CommandResult<Installation> {
  let (updates, receiver) = watch::channel(progress::initial());
  progress::monitor(
    install_bundle(target, interaction, update, included_bundles, &updates),
    receiver,
    &on_progress,
    authenticating,
  )
  .await
}

async fn install_bundle(
  target: &ConnectionTargetDto,
  interaction: &SshInteraction,
  update: &ctl_client::component_update::UpdateOptions,
  included_bundles: &[PathBuf],
  updates: &watch::Sender<Progress>,
) -> CommandResult<Installation> {
  let ConnectionTarget::Ssh {
    destination,
    options,
  } = target.to_core()
  else {
    return Err(CommandErrorDto::new(
      "invalid_ssh_target",
      "Select a remote SSH host.",
    ));
  };
  let platform_error = |error: ctl_client::CoreError| {
    CommandErrorDto::new("remote_platform_probe_failed", error.to_string())
  };
  let (platform, expected_account) = match options.remote_platform {
    RemotePlatform::Unix => {
      let output = probe_ssh_unix_platform_interactive(&destination, &options, interaction)
        .await
        .map_err(platform_error)?;
      let account = match target {
        ConnectionTargetDto::Ssh {
          remote_info: Some(identity),
          ..
        } => Some(identity.remote_id.as_str()),
        _ => None,
      };
      (parse_platform(&output)?, account)
    }
    RemotePlatform::Windows => {
      let expected_account = install_identity::verify_account(target, || {
        ctl_client::installation::remote_account_id(&destination, &options, interaction)
      })
      .await?;
      let platform = probe_ssh_windows_platform(&destination, &options, interaction)
        .await
        .map_err(platform_error)?;
      (platform, expected_account)
    }
  };
  let target_triple = platform.target_triple().map_err(bundle_error)?;
  let home = dirs::home_dir().ok_or_else(bundle_unavailable)?;
  if update.package != component_update::Package::FullBundle {
    return Err(CommandErrorDto::backend(
      "Remote updates require a complete component bundle.",
    ));
  }
  let remote = component_update::PublicationTarget {
    destination: &destination,
    options: &options,
    interaction,
    expected_account,
    target_triple,
  };
  let deployment =
    prepare_deployment(&home, &remote, &update.source, included_bundles, updates).await?;
  let manifest = match deployment {
    component_update::PublicationDeployment::Installed(manifest) => manifest,
    component_update::PublicationDeployment::Upload(prepared) => {
      component_update::install_prepared_remote_for_account(
        &destination,
        &options,
        interaction,
        &prepared,
        update.package,
        expected_account,
        |progress| {
          progress::publish(updates, progress::desktop_progress(progress));
        },
      )
      .await
      .map_err(|error| CommandErrorDto::new("remote_agent_install_failed", error.to_string()))?;
      prepared.bundle.manifest
    }
  };

  if options.remote_platform == RemotePlatform::Windows {
    ctl_client::installation::verify_remote_activation(
      &destination,
      &options,
      interaction,
      expected_account,
      &manifest,
    )
    .await
    .map_err(|error| {
      CommandErrorDto::new("remote_install_verification_failed", error.to_string())
    })?;
  }
  Ok(Installation::from_manifest(manifest, update.package))
}

impl Installation {
  fn from_manifest(
    manifest: ctl_core::bundles::Manifest,
    package: component_update::Package,
  ) -> Self {
    #[cfg(unix)]
    let agent = &manifest.components["ctl-agent"];
    Self {
      #[cfg(unix)]
      agent: agent.clone(),
      #[cfg(unix)]
      version: RemoteAgentInstallResultDto {
        app_version: agent.build.version.clone(),
        bundle_id: manifest
          .distribution_id
          .unwrap_or_else(|| manifest.bundle_id.clone()),
        git_revision: agent.build.source_revision.clone().unwrap_or_default(),
        target_triple: manifest.target_triple.clone(),
      },
      result: component_update::UpdateResult {
        package,
        bundle_id: manifest.bundle_id,
        target_triple: manifest.target_triple,
        services_preserved: true,
      },
    }
  }
}

async fn prepare_deployment(
  home: &std::path::Path,
  remote: &component_update::PublicationTarget<'_>,
  source: &BuildSource,
  included_bundles: &[PathBuf],
  updates: &watch::Sender<Progress>,
) -> CommandResult<component_update::PublicationDeployment> {
  let report = |next| progress::publish(updates, preparation_progress(next));
  let report_stage = |stage| progress::publish(updates, progress::publication_progress(stage));
  let resolved;
  let source = if matches!(source, BuildSource::Automatic) {
    resolved = automatic_remote_source(remote).await?;
    &resolved
  } else {
    source
  };
  if let BuildSource::Published { channel } = source {
    return component_update::deploy_remote_publication(
      home,
      remote,
      *channel,
      report_stage,
      report,
    )
    .await
    .map_err(CommandErrorDto::backend);
  }
  if let BuildSource::Release { publication_id } = source {
    return component_update::deploy_remote_release(
      home,
      remote,
      publication_id,
      report_stage,
      report,
    )
    .await
    .map_err(CommandErrorDto::backend);
  }
  let prepared = if matches!(source, BuildSource::WorkingTree) {
    let workspace = component_update::development_workspace().ok_or_else(|| {
      CommandErrorDto::backend("The source checkout used to build this app is unavailable.")
    })?;
    component_update::prepare_working_tree_update(
      home,
      remote.target_triple,
      Some(&workspace),
      report,
    )
    .await
  } else {
    component_update::prepare_update(
      home,
      remote.target_triple,
      ctl_core::bundles::Purpose::Upload,
      source,
      included_bundles,
      report,
    )
    .await
  }
  .map_err(CommandErrorDto::backend)?;
  Ok(component_update::PublicationDeployment::Upload(Box::new(
    prepared,
  )))
}

async fn automatic_remote_source(
  remote: &component_update::PublicationTarget<'_>,
) -> CommandResult<BuildSource> {
  use component_update::BundleChannel;
  let source = ctl_client::installation::remote_runtime_source(
    remote.destination,
    remote.options,
    remote.interaction,
    remote.expected_account,
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  let channel = match source.origin.as_ref().map(|origin| origin.channel.as_str()) {
    Some("stable") => BundleChannel::Stable,
    Some("nightly") => BundleChannel::Nightly,
    None if source.active.is_none() => BundleChannel::default(),
    _ => {
      return Err(CommandErrorDto::backend(
        "The runtime is pinned to a build. Select a publication channel or provide the build files.",
      ));
    }
  };
  Ok(BuildSource::Published { channel })
}

fn parse_platform(output: &str) -> CommandResult<Platform> {
  Platform::parse_probe(output).map_err(|_| {
    CommandErrorDto::new(
      "invalid_remote_platform",
      "The SSH host returned an invalid platform probe response.",
    )
  })
}

pub(crate) fn bundle_directories(app: &AppHandle) -> CommandResult<Vec<PathBuf>> {
  let relative = PathBuf::from("resources").join("agent-bundles");
  let packaged = app
    .path()
    .resolve(&relative, BaseDirectory::Resource)
    .map_err(CommandErrorDto::backend)?;

  #[cfg(debug_assertions)]
  {
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(&relative);
    Ok(development_bundle_directories(packaged, development))
  }

  #[cfg(not(debug_assertions))]
  {
    Ok(vec![packaged])
  }
}

#[cfg(debug_assertions)]
fn development_bundle_directories(packaged: PathBuf, development: PathBuf) -> Vec<PathBuf> {
  if packaged == development {
    vec![development]
  } else {
    // Tauri's copied resources can outlive a development bundle sync.
    vec![development, packaged]
  }
}

#[cfg(test)]
fn read_verified_bundle_sync(
  directories: &[PathBuf],
  target_triple: &str,
  expected: &ComponentBuildInfo,
) -> CommandResult<remote_bundle::VerifiedBundle> {
  remote_bundle::read_reusable_bundle(directories, target_triple, expected)
    .map_err(bundle_error)?
    .ok_or_else(bundle_unavailable)
}

fn bundle_error(error: remote_bundle::Error) -> CommandErrorDto {
  match error {
    remote_bundle::Error::NotAvailable(_) => bundle_unavailable(),
    remote_bundle::Error::Invalid(message) => bundle_invalid(message),
    remote_bundle::Error::Stale(_) => CommandErrorDto::new(
      "remote_agent_bundle_stale",
      "The remote component bundle does not match this app build. Commit component changes, run `pnpm bundles:sync` from the repository root for that exact revision, and rebuild the app before updating this host.",
    ),
    remote_bundle::Error::UnsupportedTarget(target) => CommandErrorDto::new(
      "unsupported_remote_agent_target",
      format!("No bundled ctl-agent is available for {target}."),
    ),
    error @ (remote_bundle::Error::Io(_) | remote_bundle::Error::Download(_)) => {
      CommandErrorDto::backend(error)
    }
  }
}

fn bundle_invalid(message: impl Into<String>) -> CommandErrorDto {
  CommandErrorDto::new("remote_agent_bundle_invalid", message)
}

fn bundle_unavailable() -> CommandErrorDto {
  let message = if cfg!(debug_assertions) {
    "No remote bundle set is available for development. Run `pnpm bundles:sync` from the repository root."
  } else {
    "The app package does not include its remote component bundle set."
  };
  CommandErrorDto::new("remote_agent_bundle_unavailable", message)
}

#[cfg(test)]
mod tests {
  use super::*;
  use sha2::{Digest as _, Sha256};

  const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
  const TARGET: &str = "aarch64-apple-darwin";

  fn build(source_revision: &str, dirty: bool) -> ComponentBuildInfo {
    ComponentBuildInfo {
      version: env!("CARGO_PKG_VERSION").into(),
      source_revision: (!source_revision.is_empty()).then(|| source_revision.into()),
      source_fingerprint: "a".repeat(64),
      dirty,
    }
  }

  fn manifest(archive: &[u8]) -> serde_json::Value {
    let bundle_id = format!("{}-dev.{}", env!("CARGO_PKG_VERSION"), &REVISION[..12]);
    let sha256 = format!("{:x}", Sha256::digest(archive));
    let targets = [
      "x86_64-unknown-linux-musl",
      "aarch64-unknown-linux-musl",
      "x86_64-apple-darwin",
      "aarch64-apple-darwin",
    ]
    .into_iter()
    .map(|target| {
      (
        target.to_owned(),
        serde_json::json!({
          "archive": format!("ctl-agent-bundle-{bundle_id}-{target}.tar.gz"),
          "sha256": sha256,
        }),
      )
    })
    .collect::<serde_json::Map<_, _>>();
    serde_json::json!({
      "schema_version": 1,
      "app_version": env!("CARGO_PKG_VERSION"),
      "bundle_id": bundle_id,
      "git_revision": REVISION,
      "targets": targets,
    })
  }

  struct Directory(PathBuf);

  impl Directory {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!(
        "ctmux-agent-bundle-{}",
        uuid::Uuid::new_v4().simple()
      ));
      std::fs::create_dir(&path).unwrap();
      Self(path)
    }

    fn install(&self, manifest: &serde_json::Value, archive: &[u8]) {
      std::fs::write(
        self.0.join("bundle-set.json"),
        serde_json::to_vec(manifest).unwrap(),
      )
      .unwrap();
      let name = manifest["targets"][TARGET]["archive"].as_str().unwrap();
      std::fs::write(self.0.join(name), archive).unwrap();
    }
  }

  impl Drop for Directory {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  #[cfg(debug_assertions)]
  #[test]
  fn development_bundles_precede_stale_packaged_resource_copies() {
    let packaged = PathBuf::from("target/debug/resources/agent-bundles");
    let development = PathBuf::from("src-tauri/resources/agent-bundles");

    assert_eq!(
      development_bundle_directories(packaged.clone(), development.clone()),
      vec![development.clone(), packaged]
    );
    assert_eq!(
      development_bundle_directories(development.clone(), development.clone()),
      vec![development]
    );
  }

  #[test]
  fn legacy_bundles_require_an_exact_clean_client() {
    let directory = Directory::new();
    directory.install(&manifest(b"trusted archive"), b"trusted archive");
    assert!(
      read_verified_bundle_sync(
        std::slice::from_ref(&directory.0),
        TARGET,
        &build(REVISION, false)
      )
      .is_ok()
    );
    for (source, dirty) in [
      ("a".repeat(40), false),
      (REVISION.into(), true),
      (String::new(), false),
    ] {
      let error = read_verified_bundle_sync(
        std::slice::from_ref(&directory.0),
        TARGET,
        &build(&source, dirty),
      )
      .unwrap_err();
      assert_eq!(error.code, "remote_agent_bundle_unavailable");
      assert!(error.message.contains("pnpm bundles:sync"));
    }
  }

  #[test]
  fn maps_supported_unix_platforms_to_release_targets() {
    for (os, architecture, expected) in [
      ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
      ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
      ("Darwin", "x86_64", "x86_64-apple-darwin"),
      ("Darwin", "arm64", "aarch64-apple-darwin"),
    ] {
      let platform = parse_platform(&format!("ctl-platform-v1\n{os}\n{architecture}\n")).unwrap();
      assert_eq!(platform.target_triple().unwrap(), expected);
    }
  }

  #[test]
  fn rejects_probe_noise_and_unsupported_targets() {
    let error = parse_platform("banner\nctl-platform-v1\nLinux\nx86_64\n").unwrap_err();
    assert_eq!(error.code, "invalid_remote_platform");
    let platform = parse_platform("ctl-platform-v1\nFreeBSD\nx86_64\n").unwrap();
    assert_eq!(
      platform
        .target_triple()
        .map_err(bundle_error)
        .unwrap_err()
        .code,
      "unsupported_remote_agent_target"
    );
  }

  #[test]
  fn verifies_bundle_checksum_before_installation() {
    let directory = Directory::new();
    let manifest = manifest(b"trusted archive");
    directory.install(&manifest, b"trusted archive");
    let directories = std::slice::from_ref(&directory.0);
    let expected = build(REVISION, false);
    let verified = read_verified_bundle_sync(directories, TARGET, &expected).unwrap();
    assert_eq!(verified.archive, b"trusted archive");
    assert_eq!(verified.bundle_id, manifest["bundle_id"].as_str().unwrap());
    directory.install(&manifest, b"changed archive");
    assert_eq!(
      read_verified_bundle_sync(directories, TARGET, &expected)
        .unwrap_err()
        .code,
      "remote_agent_bundle_invalid"
    );
  }

  #[test]
  fn rejects_bundle_sets_for_another_app_version() {
    let directory = Directory::new();
    let mut manifest = manifest(b"trusted archive");
    manifest["app_version"] = serde_json::json!("999.0.0");
    directory.install(&manifest, b"trusted archive");
    assert_eq!(
      read_verified_bundle_sync(
        std::slice::from_ref(&directory.0),
        TARGET,
        &build(REVISION, false)
      )
      .unwrap_err()
      .code,
      "remote_agent_bundle_invalid"
    );
  }

  #[test]
  fn distinguishes_missing_bundle_sets_from_unreadable_selected_archives() {
    let directory = Directory::new();
    let directories = std::slice::from_ref(&directory.0);
    let expected = build(REVISION, false);
    assert_eq!(
      read_verified_bundle_sync(directories, TARGET, &expected)
        .unwrap_err()
        .code,
      "remote_agent_bundle_unavailable"
    );
    let manifest = manifest(b"trusted archive");
    directory.install(&manifest, b"trusted archive");
    let archive = manifest["targets"][TARGET]["archive"].as_str().unwrap();
    std::fs::remove_file(directory.0.join(archive)).unwrap();
    assert_eq!(
      read_verified_bundle_sync(directories, TARGET, &expected)
        .unwrap_err()
        .code,
      "backend_error"
    );
  }

  #[test]
  fn skips_a_stale_legacy_development_copy_before_a_valid_packaged_copy() {
    let stale = Directory::new();
    let valid = Directory::new();
    let mut outdated = manifest(b"trusted archive");
    outdated["git_revision"] = serde_json::json!("a".repeat(40));
    outdated["bundle_id"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    for (target, metadata) in outdated["targets"].as_object_mut().unwrap().iter_mut() {
      metadata["archive"] = serde_json::json!(format!(
        "ctl-agent-bundle-{}-{}.tar.gz",
        env!("CARGO_PKG_VERSION"),
        target
      ));
    }
    stale.install(&outdated, b"trusted archive");
    valid.install(&manifest(b"trusted archive"), b"trusted archive");
    assert_eq!(
      read_verified_bundle_sync(
        &[stale.0.clone(), valid.0.clone()],
        TARGET,
        &build(REVISION, false)
      )
      .unwrap()
      .git_revision,
      REVISION
    );
  }
}
