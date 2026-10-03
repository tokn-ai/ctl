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
  local_use: BundleUse,
  upload_use: BundleUse,
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
pub async fn get_component_bundles() -> CommandResult<BundleSnapshot> {
  #[cfg(unix)]
  {
    let home = home()?;
    tokio::task::spawn_blocking(move || snapshot(&home))
      .await
      .map_err(CommandErrorDto::backend)
  }
  #[cfg(not(unix))]
  Ok(BundleSnapshot {
    errors: vec!["Component bundle selection requires a Unix host.".into()],
    ..BundleSnapshot::default()
  })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn select_component_bundle(
  request: SelectionRequest,
  on_progress: Channel<SelectionPhase>,
) -> CommandResult<SelectionResult> {
  #[cfg(unix)]
  {
    use ctl_core::bundles::{Purpose, Store};
    let home = home()?;
    let purpose = match request.purpose {
      BundlePurpose::Local => Purpose::Local,
      BundlePurpose::Upload => Purpose::Upload,
    };
    let _ = on_progress.send(SelectionPhase::Verifying);
    let bundle = {
      let home = home.clone();
      let target = request.target_triple.clone();
      let id = request.bundle_id.clone();
      tokio::task::spawn_blocking(move || Store::new(&home).get(&target, &id))
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
    let _ = (request, on_progress);
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
fn snapshot(home: &std::path::Path) -> BundleSnapshot {
  use ctl_core::bundles::{Purpose, Source, Store};
  let native = ctl_core::paths::native_target();
  let targets: std::collections::BTreeSet<_> = [
    native,
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
    let selected = |purpose| match store.selected(purpose, target) {
      Ok(bundle) => bundle.map(|bundle| bundle.manifest.bundle_id),
      Err(error) => {
        snapshot
          .errors
          .push(format!("{target} {purpose:?} selection: {error}"));
        None
      }
    };
    let mut selected = selected;
    let local = selected(Purpose::Local);
    let upload = selected(Purpose::Upload);
    let bundles = match store.list(target) {
      Ok(bundles) => bundles,
      Err(error) => {
        snapshot.errors.push(format!("{target}: {error}"));
        continue;
      }
    };
    for bundle in bundles {
      let manifest = bundle.manifest;
      let build = &manifest.components["ctl-agent"].build;
      let compatible = ctl_client::components::compatible(&manifest.components);
      let signed_package = !cfg!(target_os = "macos")
        || (manifest.files.contains_key("ctld.app/Contents/MacOS/ctld")
          && manifest.files.contains_key("ctld-package.json"));
      snapshot.bundles.push(BundleSummary {
        local_use: BundleUse::status(
          local.as_deref() == Some(&manifest.bundle_id),
          compatible && ctl_core::bundles::local_target(target) && signed_package,
        ),
        upload_use: BundleUse::status(
          upload.as_deref() == Some(&manifest.bundle_id),
          compatible && ctl_client::components::upload_target(target),
        ),
        compatible,
        app_version: build.version.clone(),
        git_revision: build.source_revision.clone(),
        dirty: build.dirty,
        source: match manifest.source {
          Source::Ci => "ci",
          Source::Release => "release",
          Source::Local => "local",
        },
        bundle_id: manifest.bundle_id,
        target_triple: manifest.target_triple,
      });
    }
  }
  snapshot
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;

  #[test]
  fn absent_store_is_passive_and_selection_fields_are_snake_case() {
    let home = std::env::temp_dir().join(format!("ctmux-bundle-snapshot-{}", uuid::Uuid::new_v4()));
    let value = serde_json::to_value(snapshot(&home)).unwrap();
    assert_eq!(value, serde_json::json!({"bundles": [], "errors": []}));
    assert!(!home.exists());
    let request: SelectionRequest = serde_json::from_value(serde_json::json!({
      "bundle_id": "id", "target_triple": "target", "purpose": "upload"
    }))
    .unwrap();
    assert!(matches!(request.purpose, BundlePurpose::Upload));
    assert!(
      serde_json::from_value::<SelectionRequest>(serde_json::json!({
        "bundleId": "id", "targetTriple": "target", "purpose": "upload"
      }))
      .is_err()
    );
  }
}
