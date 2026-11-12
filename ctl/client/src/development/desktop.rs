use super::{MAX_DESCRIPTOR_BYTES, run_builder};
use serde::Deserialize;
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

/// A desktop package built from the current source checkout.
pub struct DesktopBuild {
  pub app_version: String,
  pub bundle_id: String,
  pub archive: PathBuf,
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
pub async fn build_desktop(caller: &Path, output: &Path) -> io::Result<DesktopBuild> {
  let workspace = checkout(caller)?;
  std::fs::DirBuilder::new().mode(0o700).create(output)?;
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
  run_builder(command).await?;
  let bytes = crate::components::read_input(&output.join("result.json"), MAX_DESCRIPTOR_BYTES)?;
  let result: BuildResult = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  let name = if cfg!(target_os = "macos") {
    "ctmux.app.tar.gz"
  } else {
    "ctmux.AppImage"
  };
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
  if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 512 * 1024 * 1024 {
    return Err(io::Error::other("invalid local desktop package"));
  }
  Ok(DesktopBuild {
    app_version: result.app_version,
    bundle_id: result.bundle_id,
    archive,
  })
}

fn checkout(caller: &Path) -> io::Result<PathBuf> {
  caller
    .canonicalize()?
    .ancestors()
    .find(|directory| {
      directory.join("Cargo.toml").is_file()
        && directory
          .join("apps/desktop/src-tauri/tauri.conf.json")
          .is_file()
        && directory
          .join("scripts/dev/build-desktop-app.mts")
          .is_file()
    })
    .map(Path::to_owned)
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        "run ctlup app --local from a ctl source checkout",
      )
    })
}
