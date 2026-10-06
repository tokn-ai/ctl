//! Explicit selections shared by local discovery and remote component uploads.

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

use crate::error::{CommandErrorDto, CommandResult};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BundlePurpose {
  Local,
  Upload,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionRequest {
  bundle_id: String,
  target_triple: String,
  purpose: BundlePurpose,
}

#[derive(Debug, Serialize)]
pub struct BundleSummary {
  bundle_id: String,
  target_triple: String,
  source: &'static str,
  app_version: String,
  git_revision: Option<String>,
  dirty: bool,
  compatible: bool,
  included: bool,
  local_use: BundleUse,
  upload_use: BundleUse,
  local_unavailable_reason: Option<&'static str>,
  upload_unavailable_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum BundleUse {
  Selected,
  Available,
  Unavailable,
}

impl BundleUse {
  fn status(selected: bool, available: bool) -> Self {
    if selected {
      Self::Selected
    } else if available {
      Self::Available
    } else {
      Self::Unavailable
    }
  }
}

#[derive(Debug, Default, Serialize)]
pub struct BundleSnapshot {
  bundles: Vec<BundleSummary>,
  errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPhase {
  Verifying,
  Selecting,
}

#[derive(Debug, Serialize)]
pub struct SelectionResult {
  bundle_id: String,
  target_triple: String,
  purpose: BundlePurpose,
  services_preserved: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_component_bundles(app: tauri::AppHandle) -> CommandResult<BundleSnapshot> {
  #[cfg(unix)]
  {
    let home = home()?;
    let directories = crate::remote_agent::bundle_directories(&app)?;
    tokio::task::spawn_blocking(move || snapshot(&home, &directories))
      .await
      .map_err(CommandErrorDto::backend)
  }
  #[cfg(not(unix))]
  let _ = app;
  #[cfg(not(unix))]
  Ok(BundleSnapshot {
    errors: vec!["Component bundle selection requires a Unix host.".into()],
    ..BundleSnapshot::default()
  })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn select_component_bundle(
  app: tauri::AppHandle,
  request: SelectionRequest,
  on_progress: Channel<SelectionPhase>,
) -> CommandResult<SelectionResult> {
  #[cfg(unix)]
  {
    use ctl_core::bundles::Purpose;
    let home = home()?;
    let purpose = match request.purpose {
      BundlePurpose::Local => Purpose::Local,
      BundlePurpose::Upload => Purpose::Upload,
    };
    let directories = crate::remote_agent::bundle_directories(&app)?;
    let _ = on_progress.send(SelectionPhase::Verifying);
    let bundle = {
      let home = home.clone();
      let target = request.target_triple.clone();
      let id = request.bundle_id.clone();
      tokio::task::spawn_blocking(move || load_selection(&home, &directories, &target, &id))
        .await
        .map_err(CommandErrorDto::backend)?
        .map_err(selection_error)?
    };
    if purpose == Purpose::Upload
      && !ctl_client::components::upload_target(&bundle.manifest.target_triple)
    {
      return Err(selection_error(
        "This target is unavailable for remote uploads.",
      ));
    }
    ctl_client::components::select_with_progress(&home, purpose, &bundle, || {
      let _ = on_progress.send(SelectionPhase::Selecting);
    })
    .await
    .map_err(selection_error)?;
    Ok(SelectionResult {
      bundle_id: request.bundle_id,
      target_triple: request.target_triple,
      purpose: request.purpose,
      services_preserved: true,
    })
  }
  #[cfg(not(unix))]
  {
    let _ = (app, request, on_progress);
    Err(CommandErrorDto::new(
      "component_bundles_unsupported",
      "Component bundle selection requires a Unix host.",
    ))
  }
}

#[cfg(unix)]
fn home() -> CommandResult<std::path::PathBuf> {
  dirs::home_dir()
    .ok_or_else(|| CommandErrorDto::new("home_unavailable", "Home directory is unavailable."))
}

#[cfg(unix)]
fn selection_error(error: impl std::fmt::Display) -> CommandErrorDto {
  CommandErrorDto::new("component_bundle_selection_failed", error.to_string())
}

#[cfg(unix)]
fn snapshot(home: &std::path::Path, directories: &[std::path::PathBuf]) -> BundleSnapshot {
  snapshot_with(home, |target| {
    included_bundle(directories, target).map(|bundle| bundle.map(|bundle| bundle.manifest))
  })
}

#[cfg(unix)]
struct IncludedBundle {
  archive: ctl_client::remote_bundle::VerifiedBundle,
  manifest: ctl_core::bundles::Manifest,
}

#[cfg(unix)]
fn included_bundle(
  directories: &[std::path::PathBuf],
  target: &str,
) -> Result<Option<IncludedBundle>, ctl_client::remote_bundle::Error> {
  use ctl_core::bundles::Source;
  if !ctl_client::components::upload_target(target) {
    return Ok(None);
  }
  let Some(bundle) = ctl_client::remote_bundle::read_compatible_bundle(directories, target)? else {
    return Ok(None);
  };
  let source = if bundle.bundle_id == bundle.app_version {
    Source::Release
  } else {
    Source::Ci
  };
  let manifest = ctl_client::components::inspect_remote(&bundle, target, source)?;
  Ok(Some(IncludedBundle {
    archive: bundle,
    manifest,
  }))
}

#[cfg(unix)]
fn load_selection(
  home: &std::path::Path,
  directories: &[std::path::PathBuf],
  target: &str,
  id: &str,
) -> std::io::Result<ctl_core::bundles::Bundle> {
  use ctl_core::bundles::Store;
  match Store::new(home).get(target, id) {
    Ok(bundle) => return Ok(bundle),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
    Err(error) => return Err(error),
  }
  let included = included_bundle(directories, target)
    .map_err(std::io::Error::other)?
    .filter(|bundle| bundle.manifest.bundle_id == id)
    .ok_or_else(|| {
      std::io::Error::other(
        "This included build changed or is unavailable. Refresh bundles and choose again.",
      )
    })?;
  ctl_client::components::import_remote(home, &included.archive, target, included.manifest.source)
    .map_err(std::io::Error::other)
}

#[cfg(unix)]
fn snapshot_with(
  home: &std::path::Path,
  mut included: impl FnMut(
    &str,
  ) -> Result<
    Option<ctl_core::bundles::Manifest>,
    ctl_client::remote_bundle::Error,
  >,
) -> BundleSnapshot {
  use ctl_core::bundles::{Purpose, Store};
  let targets: std::collections::BTreeSet<_> = [
    ctl_core::paths::native_target(),
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
  ]
  .into_iter()
  .collect();
  let store = Store::new(home);
  let mut snapshot = BundleSnapshot::default();
  for target in targets {
    let mut selected = |purpose| match store.selected(purpose, target) {
      Ok(bundle) => bundle.map(|bundle| bundle.manifest.bundle_id),
      Err(error) => {
        snapshot
          .errors
          .push(format!("{target} {purpose:?} selection: {error}"));
        None
      }
    };
    let local = selected(Purpose::Local);
    let upload = selected(Purpose::Upload);
    match store.list(target) {
      Ok(bundles) => snapshot.bundles.extend(
        bundles
          .iter()
          .map(|bundle| summary(&bundle.manifest, local.as_deref(), upload.as_deref(), false)),
      ),
      Err(error) => snapshot.errors.push(format!("{target}: {error}")),
    }
    match included(target) {
      Ok(Some(manifest)) => {
        if let Some(stored) = snapshot
          .bundles
          .iter_mut()
          .find(|bundle| bundle.target_triple == target && bundle.bundle_id == manifest.bundle_id)
        {
          stored.included = true;
        } else {
          snapshot.bundles.push(summary(
            &manifest,
            local.as_deref(),
            upload.as_deref(),
            true,
          ));
        }
      }
      Ok(None) => {}
      Err(error) => snapshot
        .errors
        .push(format!("Included {target} bundle: {error}")),
    }
  }
  snapshot
}

#[cfg(unix)]
fn summary(
  manifest: &ctl_core::bundles::Manifest,
  local: Option<&str>,
  upload: Option<&str>,
  included: bool,
) -> BundleSummary {
  use ctl_core::bundles::Source;
  let target = &manifest.target_triple;
  let build = &manifest.components["ctl-agent"].build;
  let compatible = ctl_client::components::compatible(&manifest.components);
  let signed_package = !cfg!(target_os = "macos")
    || (manifest.files.contains_key("ctld.app/Contents/MacOS/ctld")
      && manifest.files.contains_key("ctld-package.json"));
  let local_reason = if !compatible {
    Some("Incompatible with this app")
  } else if !ctl_core::bundles::local_target(target) {
    Some("For another platform")
  } else if !signed_package {
    Some("Requires a signed macOS helper package")
  } else {
    None
  };
  let upload_reason = if !compatible {
    Some("Incompatible with this app")
  } else if !ctl_client::components::upload_target(target) {
    Some("This target does not support remote uploads")
  } else {
    None
  };
  BundleSummary {
    local_use: BundleUse::status(local == Some(&manifest.bundle_id), local_reason.is_none()),
    upload_use: BundleUse::status(upload == Some(&manifest.bundle_id), upload_reason.is_none()),
    local_unavailable_reason: local_reason,
    upload_unavailable_reason: upload_reason,
    compatible,
    included,
    app_version: build.version.clone(),
    git_revision: build.source_revision.clone(),
    dirty: build.dirty,
    source: match manifest.source {
      Source::Ci => "ci",
      Source::Release => "release",
      Source::Local => "local",
    },
    bundle_id: manifest.bundle_id.clone(),
    target_triple: target.clone(),
  }
}

#[cfg(all(test, unix))]
mod tests;
