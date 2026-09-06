use std::path::{Path, PathBuf};

use ctl_core::{
  SshConnectionOptions, SshInteraction, install_ssh_unix_agent_interactive,
  probe_ssh_unix_platform_interactive,
};
use sha2::{Digest as _, Sha256};
use tauri::{AppHandle, Manager as _, path::BaseDirectory};

use crate::dto::RemoteAgentInstallResultDto;
use crate::error::{CommandErrorDto, CommandResult};

const MAX_BUNDLE_BYTES: usize = 128 * 1024 * 1024;
const PLATFORM_MARKER: &str = "ctl-platform-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePlatform {
  os: String,
  architecture: String,
}

pub async fn install(
  app: &AppHandle,
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> CommandResult<RemoteAgentInstallResultDto> {
  let platform = probe_ssh_unix_platform_interactive(destination, options, interaction)
    .await
    .map_err(|error| CommandErrorDto::new("remote_platform_probe_failed", error.to_string()))?;
  let platform = parse_platform(&platform)?;
  let target_triple = platform.target_triple()?;
  let version = env!("CARGO_PKG_VERSION");
  let file_name = format!("ctl-agent-bundle-{version}-{target_triple}.tar.gz");
  let bundle_path = resolve_bundle(app, &file_name)?;
  let checksum_path = resolve_bundle(app, &format!("{file_name}.sha256"))?;
  let archive = read_verified_bundle(bundle_path, checksum_path).await?;

  install_ssh_unix_agent_interactive(destination, options, interaction, version, &archive)
    .await
    .map_err(|error| CommandErrorDto::new("remote_agent_install_failed", error.to_string()))?;

  Ok(RemoteAgentInstallResultDto {
    version: version.into(),
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

fn resolve_bundle(app: &AppHandle, file_name: &str) -> CommandResult<PathBuf> {
  let relative = PathBuf::from("resources")
    .join("agent-bundles")
    .join(file_name);
  let packaged = app
    .path()
    .resolve(&relative, BaseDirectory::Resource)
    .map_err(CommandErrorDto::backend)?;
  if packaged.is_file() {
    return Ok(packaged);
  }

  #[cfg(debug_assertions)]
  {
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(&relative);
    if development.is_file() {
      return Ok(development);
    }
  }

  Err(CommandErrorDto::new(
    "remote_agent_bundle_unavailable",
    format!("The app does not include {file_name}."),
  ))
}

async fn read_verified_bundle(
  bundle_path: PathBuf,
  checksum_path: PathBuf,
) -> CommandResult<Vec<u8>> {
  tokio::task::spawn_blocking(move || read_verified_bundle_sync(&bundle_path, &checksum_path))
    .await
    .map_err(CommandErrorDto::backend)?
}

fn read_verified_bundle_sync(bundle_path: &Path, checksum_path: &Path) -> CommandResult<Vec<u8>> {
  let archive = std::fs::read(bundle_path).map_err(CommandErrorDto::backend)?;
  if archive.len() > MAX_BUNDLE_BYTES {
    return Err(CommandErrorDto::new(
      "remote_agent_bundle_invalid",
      "The bundled ctl-agent archive exceeds the size limit.",
    ));
  }
  let checksum = std::fs::read_to_string(checksum_path).map_err(CommandErrorDto::backend)?;
  let expected = checksum
    .split_ascii_whitespace()
    .next()
    .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .ok_or_else(|| {
      CommandErrorDto::new(
        "remote_agent_bundle_invalid",
        "The bundled ctl-agent checksum is invalid.",
      )
    })?;
  let actual = format!("{:x}", Sha256::digest(&archive));
  if !actual.eq_ignore_ascii_case(expected) {
    return Err(CommandErrorDto::new(
      "remote_agent_bundle_invalid",
      "The bundled ctl-agent archive failed checksum verification.",
    ));
  }
  Ok(archive)
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
    let bundle = directory.join("bundle.tar.gz");
    let checksum = directory.join("bundle.tar.gz.sha256");
    std::fs::write(&bundle, b"trusted archive").unwrap();
    std::fs::write(
      &checksum,
      format!("{:x}  bundle.tar.gz\n", Sha256::digest(b"trusted archive")),
    )
    .unwrap();
    assert_eq!(
      read_verified_bundle_sync(&bundle, &checksum).unwrap(),
      b"trusted archive"
    );
    std::fs::write(&bundle, b"changed archive").unwrap();
    assert_eq!(
      read_verified_bundle_sync(&bundle, &checksum)
        .unwrap_err()
        .code,
      "remote_agent_bundle_invalid"
    );
    std::fs::remove_dir_all(directory).unwrap();
  }
}
