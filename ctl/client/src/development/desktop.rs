use super::{MAX_DESCRIPTOR_BYTES, run_recipe};
use ctl_core::private_fs;
use serde::Deserialize;
use std::io;
use std::path::{Path, PathBuf};

/// A desktop package built from the current source checkout.
pub struct DesktopBuild {
  pub app_version: String,
  pub bundle_id: String,
  pub archive: PathBuf,
  pub source_revision: Option<String>,
  pub built_at: Option<String>,
  pub installation_layout_version: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildResult {
  schema_version: u16,
  app_version: String,
  bundle_id: String,
  archive: String,
}

/// Builds a native desktop package into a fresh private output directory.
/// # Errors
/// Reports a missing checkout, build failures, or invalid output metadata.
pub async fn build_desktop(workspace: &Path, output: &Path) -> io::Result<DesktopBuild> {
  let workspace = checkout(workspace)?;
  let source_revision = checkout_revision(&workspace).await;
  private_fs::create_directory(output)?;
  let output = output.canonicalize()?;
  let mut command = tokio::process::Command::new(
    std::env::var_os("CTL_NODE_PROGRAM").unwrap_or_else(|| "node".into()),
  );
  command
    .args([
      "--experimental-strip-types",
      "--disable-warning=ExperimentalWarning",
    ])
    .arg(workspace.join("scripts/dev/build-desktop-app.mts"))
    .arg("--workspace")
    .arg(&workspace)
    .arg("--output")
    .arg(&output)
    .current_dir(&workspace)
    .env("CTLUP_DEVELOPMENT_LIFELINE", "1")
    .env_remove("CTMUX_DEV_APP_SUPERVISOR")
    .env_remove("CTMUX_DEV_CTLD_EXECUTABLE")
    .env_remove("CTMUX_DEV_APP_FAILURE_MARKER");
  run_recipe(command, "desktop development build").await?;
  let mut built = read_output(&output, archive_name())?;
  built.source_revision = source_revision;
  built.built_at = time::OffsetDateTime::now_utc()
    .format(&time::format_description::well_known::Rfc3339)
    .ok();
  Ok(built)
}

async fn checkout_revision(workspace: &Path) -> Option<String> {
  if !workspace.join(".git").exists() {
    return None;
  }
  let result = tokio::process::Command::new("git")
    .args(["rev-parse", "--verify", "HEAD"])
    .current_dir(workspace)
    .stdin(std::process::Stdio::null())
    .output()
    .await
    .ok()?;
  let text = std::str::from_utf8(&result.stdout).ok()?.trim();
  (result.status.success() && text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| text.to_owned())
}

fn archive_name() -> &'static str {
  if cfg!(target_os = "macos") {
    "ctmux.app.tar.gz"
  } else if cfg!(windows) {
    "ctmux.zip"
  } else {
    "ctmux.AppImage"
  }
}

fn read_output(output: &Path, name: &str) -> io::Result<DesktopBuild> {
  let bytes = crate::components::read_input(&output.join("result.json"), MAX_DESCRIPTOR_BYTES)?;
  let result: BuildResult = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  if result.schema_version != 1
    || !crate::remote_bundle::safe_id(&result.app_version)
    || !result
      .bundle_id
      .strip_prefix("local.")
      .is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
      })
    || result.archive != name
  {
    return Err(io::Error::other("invalid local desktop build result"));
  }
  let archive = output.join(name);
  let metadata = std::fs::symlink_metadata(&archive)?;
  if !private_fs::is_owned_file(&metadata)
    || metadata.len() == 0
    || metadata.len() > 512 * 1024 * 1024
  {
    return Err(io::Error::other("invalid local desktop package"));
  }
  let installation_layout_version =
    match crate::components::read_input(&output.join("installation-layout-version"), 8) {
      Ok(bytes) => Some(
        std::str::from_utf8(&bytes)
          .ok()
          .and_then(|value| value.trim().parse().ok())
          .ok_or_else(|| io::Error::other("invalid desktop installation layout version"))?,
      ),
      Err(error) if error.kind() == io::ErrorKind::NotFound => None,
      Err(error) => return Err(error),
    };
  Ok(DesktopBuild {
    app_version: result.app_version,
    bundle_id: result.bundle_id,
    archive,
    source_revision: None,
    built_at: None,
    installation_layout_version,
  })
}

fn checkout(workspace: &Path) -> io::Result<PathBuf> {
  let workspace = workspace.canonicalize()?;
  if [
    "Cargo.toml",
    "apps/desktop/src-tauri/tauri.conf.json",
    "scripts/dev/build-desktop-app.mts",
  ]
  .iter()
  .all(|file| workspace.join(file).is_file())
  {
    Ok(workspace)
  } else {
    Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "desktop development builds require a ctl checkout root",
    ))
  }
}

#[cfg(test)]
mod tests;
