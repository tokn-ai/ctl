use std::collections::BTreeMap;
use std::path::PathBuf;

use ctl_core::{
  RemoteInstallEvent, SshConnectionOptions, SshInteraction,
  install_ssh_unix_agent_interactive_with_progress, probe_ssh_unix_platform_interactive,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tauri::{AppHandle, Manager as _, ipc::Channel, path::BaseDirectory};
use tokio::sync::watch;

use crate::dto::{
  RemoteAgentInstallPhase as Phase, RemoteAgentInstallProgressDto as Progress,
  RemoteAgentInstallResultDto,
};
use crate::error::{CommandErrorDto, CommandResult};

mod progress;

const MAX_BUNDLE_BYTES: usize = 128 * 1024 * 1024;
const MAX_BUNDLE_SET_BYTES: usize = 64 * 1024;
const BUNDLE_SET_SCHEMA_VERSION: u32 = 1;
const BUNDLE_SET_FILE: &str = "bundle-set.json";
const PLATFORM_MARKER: &str = "ctl-platform-v1";
const SUPPORTED_TARGETS: [&str; 4] = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePlatform {
  os: String,
  architecture: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleSetManifest {
  schema_version: u32,
  app_version: String,
  bundle_id: String,
  git_revision: String,
  targets: BTreeMap<String, BundleTargetManifest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleTargetManifest {
  archive: String,
  sha256: String,
}

#[derive(Debug)]
struct VerifiedBundle {
  app_version: String,
  bundle_id: String,
  git_revision: String,
  archive: Vec<u8>,
  file_name: String,
}

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
  let target_triple = platform.target_triple()?;
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

impl RemotePlatform {
  fn target_triple(&self) -> CommandResult<&'static str> {
    match (self.os.as_str(), self.architecture.as_str()) {
      ("Linux", "x86_64" | "amd64") => Ok("x86_64-unknown-linux-musl"),
      ("Linux", "aarch64" | "arm64") => Ok("aarch64-unknown-linux-musl"),
      ("Darwin", "x86_64" | "amd64") => Ok("x86_64-apple-darwin"),
      ("Darwin", "aarch64" | "arm64") => Ok("aarch64-apple-darwin"),
      _ => Err(CommandErrorDto::new(
        "unsupported_remote_agent_target",
        format!(
          "No bundled ctl-agent is available for {} {}.",
          self.os, self.architecture
        ),
      )),
    }
  }
}

fn parse_platform(output: &str) -> CommandResult<RemotePlatform> {
  let mut lines = output.lines();
  let marker = lines.next().map(str::trim_end);
  let os = lines
    .next()
    .map(str::trim)
    .filter(|value| !value.is_empty());
  let architecture = lines
    .next()
    .map(str::trim)
    .filter(|value| !value.is_empty());
  if marker != Some(PLATFORM_MARKER)
    || os.is_none()
    || architecture.is_none()
    || lines.any(|line| !line.trim().is_empty())
  {
    return Err(CommandErrorDto::new(
      "invalid_remote_platform",
      "The SSH host returned an invalid platform probe response.",
    ));
  }
  Ok(RemotePlatform {
    os: os.unwrap().into(),
    architecture: architecture.unwrap().into(),
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
    if packaged != development {
      return Ok(vec![packaged, development]);
    }
  }

  Ok(vec![packaged])
}

async fn read_verified_bundle(
  directories: Vec<PathBuf>,
  target_triple: &str,
) -> CommandResult<VerifiedBundle> {
  let target_triple = target_triple.to_owned();
  tokio::task::spawn_blocking(move || read_verified_bundle_sync(&directories, &target_triple))
    .await
    .map_err(CommandErrorDto::backend)?
}

fn read_verified_bundle_sync(
  directories: &[PathBuf],
  target_triple: &str,
) -> CommandResult<VerifiedBundle> {
  let directory = directories
    .iter()
    .find(|directory| directory.join(BUNDLE_SET_FILE).is_file())
    .ok_or_else(bundle_unavailable)?;
  let manifest_path = directory.join(BUNDLE_SET_FILE);
  let manifest_bytes = std::fs::read(&manifest_path).map_err(CommandErrorDto::backend)?;
  if manifest_bytes.len() > MAX_BUNDLE_SET_BYTES {
    return Err(bundle_invalid(
      "The remote bundle-set manifest is too large.",
    ));
  }
  let manifest = parse_bundle_set(&manifest_bytes)?;
  let target = manifest.targets.get(target_triple).ok_or_else(|| {
    bundle_invalid(format!(
      "The remote bundle set does not contain {target_triple}."
    ))
  })?;
  let bundle_path = directory.join(&target.archive);
  let archive = std::fs::read(bundle_path).map_err(CommandErrorDto::backend)?;
  if archive.len() > MAX_BUNDLE_BYTES {
    return Err(bundle_invalid(
      "The bundled ctl-agent archive exceeds the size limit.",
    ));
  }
  let actual = format!("{:x}", Sha256::digest(&archive));
  if !actual.eq_ignore_ascii_case(&target.sha256) {
    return Err(bundle_invalid(
      "The bundled ctl-agent archive failed checksum verification.",
    ));
  }
  Ok(VerifiedBundle {
    app_version: manifest.app_version,
    bundle_id: manifest.bundle_id,
    git_revision: manifest.git_revision,
    archive,
    file_name: target.archive.clone(),
  })
}

fn parse_bundle_set(bytes: &[u8]) -> CommandResult<BundleSetManifest> {
  let manifest: BundleSetManifest = serde_json::from_slice(bytes)
    .map_err(|_| bundle_invalid("The remote bundle-set manifest is invalid JSON."))?;
  if manifest.schema_version != BUNDLE_SET_SCHEMA_VERSION {
    return Err(bundle_invalid(
      "The remote bundle-set schema version is unsupported.",
    ));
  }
  if manifest.app_version != env!("CARGO_PKG_VERSION") {
    return Err(bundle_invalid(format!(
      "The remote bundle set targets app version {}, not {}.",
      manifest.app_version,
      env!("CARGO_PKG_VERSION")
    )));
  }
  if !is_safe_bundle_id(&manifest.bundle_id) {
    return Err(bundle_invalid("The remote bundle id is invalid."));
  }
  if manifest.git_revision.len() != 40
    || !manifest
      .git_revision
      .bytes()
      .all(|byte| byte.is_ascii_hexdigit())
  {
    return Err(bundle_invalid("The remote bundle git revision is invalid."));
  }
  let development_bundle_id = format!(
    "{}-dev.{}",
    manifest.app_version,
    &manifest.git_revision[..12]
  );
  if manifest.bundle_id != manifest.app_version && manifest.bundle_id != development_bundle_id {
    return Err(bundle_invalid(
      "The remote bundle id does not match its version and Git revision.",
    ));
  }
  if manifest.targets.len() != SUPPORTED_TARGETS.len()
    || SUPPORTED_TARGETS
      .iter()
      .any(|target| !manifest.targets.contains_key(*target))
  {
    return Err(bundle_invalid(
      "The remote bundle set does not contain every supported target.",
    ));
  }
  for (target_triple, target) in &manifest.targets {
    let expected_archive = format!(
      "ctl-agent-bundle-{}-{target_triple}.tar.gz",
      manifest.bundle_id
    );
    if target.archive != expected_archive
      || target.sha256.len() != 64
      || !target.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
      return Err(bundle_invalid(format!(
        "The remote bundle metadata for {target_triple} is invalid."
      )));
    }
  }
  Ok(manifest)
}

fn is_safe_bundle_id(bundle_id: &str) -> bool {
  !bundle_id.is_empty()
    && bundle_id.len() <= 128
    && bundle_id
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
}

fn bundle_invalid(message: impl Into<String>) -> CommandErrorDto {
  CommandErrorDto::new("remote_agent_bundle_invalid", message)
}

fn bundle_unavailable() -> CommandErrorDto {
  let message = if cfg!(debug_assertions) {
    "No remote bundle set is available for development. Run `pnpm agents:sync` from apps/rmux."
  } else {
    "The app package does not include its remote component bundle set."
  };
  CommandErrorDto::new("remote_agent_bundle_unavailable", message)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn maps_supported_unix_platforms_to_release_targets() {
    for (os, architecture, expected) in [
      ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
      ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
      ("Darwin", "x86_64", "x86_64-apple-darwin"),
      ("Darwin", "arm64", "aarch64-apple-darwin"),
    ] {
      let platform = parse_platform(&format!("{PLATFORM_MARKER}\n{os}\n{architecture}\n")).unwrap();
      assert_eq!(platform.target_triple().unwrap(), expected);
    }
  }

  #[test]
  fn rejects_probe_noise_and_unsupported_targets() {
    assert!(parse_platform("banner\nctl-platform-v1\nLinux\nx86_64\n").is_err());
    let platform = parse_platform("ctl-platform-v1\nFreeBSD\nx86_64\n").unwrap();
    assert_eq!(
      platform.target_triple().unwrap_err().code,
      "unsupported_remote_agent_target"
    );
  }

  #[test]
  fn verifies_bundle_checksum_before_installation() {
    let directory = std::env::temp_dir().join(format!(
      "rmux-agent-bundle-{}",
      uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&directory).unwrap();
    let bundle_id = "0.1.0-dev.0123456789ab";
    let target_triple = "aarch64-apple-darwin";
    let bundle_name = format!("ctl-agent-bundle-{bundle_id}-{target_triple}.tar.gz");
    let bundle = directory.join(&bundle_name);
    std::fs::write(&bundle, b"trusted archive").unwrap();
    let sha256 = format!("{:x}", Sha256::digest(b"trusted archive"));
    let targets = SUPPORTED_TARGETS
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
    let manifest = serde_json::json!({
      "schema_version": 1,
      "app_version": env!("CARGO_PKG_VERSION"),
      "bundle_id": bundle_id,
      "git_revision": "0123456789abcdef0123456789abcdef01234567",
      "targets": targets,
    });
    std::fs::write(
      directory.join(BUNDLE_SET_FILE),
      serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let directories = vec![directory.clone()];
    let verified = read_verified_bundle_sync(&directories, target_triple).unwrap();
    assert_eq!(verified.archive, b"trusted archive");
    assert_eq!(verified.bundle_id, bundle_id);
    std::fs::write(&bundle, b"changed archive").unwrap();
    assert_eq!(
      read_verified_bundle_sync(&directories, target_triple)
        .unwrap_err()
        .code,
      "remote_agent_bundle_invalid"
    );
    std::fs::remove_dir_all(directory).unwrap();
  }

  #[test]
  fn rejects_bundle_sets_for_another_app_version() {
    let manifest = serde_json::json!({
      "schema_version": 1,
      "app_version": "999.0.0",
      "bundle_id": "999.0.0",
      "git_revision": "0123456789abcdef0123456789abcdef01234567",
      "targets": {},
    });
    assert_eq!(
      parse_bundle_set(&serde_json::to_vec(&manifest).unwrap())
        .unwrap_err()
        .code,
      "remote_agent_bundle_invalid"
    );
  }
}
