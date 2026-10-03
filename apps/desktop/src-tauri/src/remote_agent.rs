use std::path::PathBuf;

use ctl_client::{
  RemoteInstallEvent, SshConnectionOptions, SshInteraction,
  install_ssh_unix_agent_interactive_with_progress, probe_ssh_unix_platform_interactive,
  remote_bundle::{self, Platform, VerifiedBundle},
};
use ctl_core::component::ComponentBuildInfo;
use tauri::{AppHandle, Manager as _, ipc::Channel, path::BaseDirectory};
use tokio::sync::watch;

use crate::dto::{
  RemoteAgentInstallPhase as Phase, RemoteAgentInstallProgressDto as Progress,
  RemoteAgentInstallResultDto,
};
use crate::error::{CommandErrorDto, CommandResult};

mod progress;

pub async fn install(
  app: &AppHandle,
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  on_progress: Channel<Progress>,
  authenticating: impl Fn() -> bool,
) -> CommandResult<RemoteAgentInstallResultDto> {
  let (updates, receiver) = watch::channel(progress::initial());
  progress::monitor(
    install_bundle(app, destination, options, interaction, &updates),
    receiver,
    &on_progress,
    authenticating,
  )
  .await
}

async fn install_bundle(
  app: &AppHandle,
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  updates: &watch::Sender<Progress>,
) -> CommandResult<RemoteAgentInstallResultDto> {
  let platform = probe_ssh_unix_platform_interactive(destination, options, interaction)
    .await
    .map_err(|error| CommandErrorDto::new("remote_platform_probe_failed", error.to_string()))?;
  let platform = parse_platform(&platform)?;
  let target_triple = platform.target_triple().map_err(bundle_error)?;
  updates.send_modify(|progress| progress.phase = Phase::VerifyingBundle);
  let bundle = read_verified_bundle(bundle_directories(app)?, target_triple).await?;
  updates.send_modify(|progress| {
    progress.phase = Phase::Connecting;
    progress.file_name = Some(bundle.file_name.clone());
    progress.total_bytes = bundle.archive.len() as u64;
  });

  install_ssh_unix_agent_interactive_with_progress(
    destination,
    options,
    interaction,
    &bundle.bundle_id,
    &bundle.archive,
    |event| {
      updates.send_modify(|progress| {
        progress.phase = match event {
          RemoteInstallEvent::Receiving { received_bytes } => {
            progress.transferred_bytes = received_bytes;
            Phase::Transferring
          }
          RemoteInstallEvent::Extracting => Phase::Extracting,
          RemoteInstallEvent::Checking { file_name } => {
            progress.file_name = Some(file_name.into());
            Phase::Checking
          }
          RemoteInstallEvent::Activating => {
            progress.file_name = None;
            Phase::Activating
          }
          RemoteInstallEvent::Complete => Phase::Complete,
        };
      });
    },
  )
  .await
  .map_err(|error| CommandErrorDto::new("remote_agent_install_failed", error.to_string()))?;

  Ok(RemoteAgentInstallResultDto {
    app_version: bundle.app_version,
    bundle_id: bundle.bundle_id,
    git_revision: bundle.git_revision,
    target_triple: target_triple.into(),
  })
}

fn parse_platform(output: &str) -> CommandResult<Platform> {
  Platform::parse_probe(output).map_err(|_| {
    CommandErrorDto::new(
      "invalid_remote_platform",
      "The SSH host returned an invalid platform probe response.",
    )
  })
}

fn bundle_directories(app: &AppHandle) -> CommandResult<Vec<PathBuf>> {
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

fn expected_bundle_build() -> ComponentBuildInfo {
  let mut expected = ctl_core::component::build_info();
  expected.version = env!("CARGO_PKG_VERSION").into();
  expected.source_revision =
    (!env!("CTMUX_SOURCE_REVISION").is_empty()).then(|| env!("CTMUX_SOURCE_REVISION").into());
  expected.dirty |= env!("CTMUX_COMPONENTS_DIRTY") == "true";
  expected
}

async fn read_verified_bundle(
  directories: Vec<PathBuf>,
  target_triple: &str,
) -> CommandResult<VerifiedBundle> {
  let target_triple = target_triple.to_owned();
  let expected = expected_bundle_build();
  #[cfg(unix)]
  {
    let home = dirs::home_dir().ok_or_else(bundle_unavailable)?;
    let selected = tokio::task::spawn_blocking(move || {
      let store = ctl_core::bundles::Store::new(&home);
      if let Some(bundle) = store
        .selected(ctl_core::bundles::Purpose::Upload, &target_triple)
        .map_err(CommandErrorDto::backend)?
      {
        return Ok((home, bundle, false));
      }
      let bundle = read_verified_bundle_sync(&directories, &target_triple, &expected)?;
      let source = if bundle.bundle_id == bundle.app_version {
        ctl_core::bundles::Source::Release
      } else {
        ctl_core::bundles::Source::Ci
      };
      let imported = ctl_client::components::import_remote(&home, &bundle, &target_triple, source)
        .map_err(bundle_error)?;
      Ok::<_, CommandErrorDto>((home, imported, true))
    })
    .await
    .map_err(CommandErrorDto::backend)??;
    let bundle = if selected.2 {
      ctl_client::components::initialize_upload(&selected.0, &selected.1)
        .await
        .map_err(CommandErrorDto::backend)?
    } else {
      selected.1
    };
    tokio::task::spawn_blocking(move || {
      ctl_client::components::upload_bundle(&bundle).map_err(CommandErrorDto::backend)
    })
    .await
    .map_err(CommandErrorDto::backend)?
  }
  #[cfg(not(unix))]
  tokio::task::spawn_blocking(move || {
    read_verified_bundle_sync(&directories, &target_triple, &expected)
  })
  .await
  .map_err(CommandErrorDto::backend)?
}

fn read_verified_bundle_sync(
  directories: &[PathBuf],
  target_triple: &str,
  expected: &ComponentBuildInfo,
) -> CommandResult<VerifiedBundle> {
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
      "The remote component bundle does not match this app build. Commit component changes, run `pnpm agents:sync` from apps/desktop for that exact revision, and rebuild the app before updating this host.",
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
    "No remote bundle set is available for development. Run `pnpm agents:sync` from apps/desktop."
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
      assert!(error.message.contains("pnpm agents:sync"));
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
