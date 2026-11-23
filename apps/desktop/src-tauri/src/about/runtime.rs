//! Inspect the selected runtime and let the managed ctlup follow its tracked source.

use self::manager::{ManagerAction, run_manager};
use crate::error::{CommandErrorDto, CommandResult};
use ctl_client::hosts::HostCatalogSnapshot;
use ctl_client::installation::InstallationShow;
use ctl_core::component::ComponentBuildInfo;
use ctl_core::component_update::{BundleChannel, Package, UpdateResult};
use ctl_core::installations::CatalogOrigin;
use serde::{Deserialize, Serialize};
use std::{
  collections::{BTreeMap, BTreeSet},
  io,
  time::Duration,
};
use tokio::sync::watch;

mod manager;

#[derive(Serialize)]
pub(crate) struct AboutRuntimeStatus {
  active: Option<String>,
  build_label: Option<String>,
  origin: Option<CatalogOrigin>,
  missing: bool,
  build_channels: Vec<BuildChannels>,
  target_triple: Option<String>,
  active_build: Option<ComponentBuildInfo>,
  #[serde(skip_serializing_if = "Option::is_none")]
  installed_targets: Option<Vec<InstalledTarget>>,
}

#[derive(Serialize)]
struct InstalledTarget {
  target_triple: String,
  builds: Vec<BuildChannels>,
  current_publications: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct BuildChannels {
  build: ComponentBuildInfo,
  channels: BTreeSet<String>,
  current: bool,
}

impl AboutRuntimeStatus {
  fn from_inventory(inventory: InstallationShow, native_target: Option<&str>) -> io::Result<Self> {
    inventory.validate()?;
    let active = inventory.active.as_ref();
    let active_build = inventory
      .builds
      .iter()
      .find(|build| Some(&build.bundle_id) == active);
    let build_label = active_build.map(|build| {
      ctl_core::installed_build_name::label(
        &build.build,
        inventory.active_origin.as_ref(),
        &build.bundle_id,
      )
    });
    let active_build = active_build.map(|build| build.build.clone());
    let target_triple = active
      .and_then(|id| selected_target(&inventory, id))
      .or(native_target)
      .map(str::to_owned);
    let targets = native_target.map_or_else(
      || {
        inventory
          .active
          .iter()
          .chain(&inventory.previous)
          .filter_map(|id| selected_target(&inventory, id))
          .collect()
      },
      |target| vec![target],
    );
    let build_channels = channels_for_targets(&inventory, &targets);
    let installed_targets = native_target.map(|_| installed_targets(&inventory));
    Ok(Self {
      missing: active.is_some_and(|id| inventory.missing_builds.contains(id)),
      active: inventory.active,
      build_label,
      origin: inventory.active_origin,
      build_channels,
      target_triple,
      active_build,
      installed_targets,
    })
  }
}

// A remote catalog can contain builds for many targets. Only its active or
// previous local selections establish which bundle targets run on that host.
// A missing selection can still have a target recorded in its catalog entry.
fn selected_target<'a>(inventory: &'a InstallationShow, id: &str) -> Option<&'a str> {
  inventory
    .builds
    .iter()
    .find(|build| build.bundle_id == id)
    .map(|build| build.target_triple.as_str())
    .or_else(|| {
      inventory
        .installations
        .iter()
        .find(|entry| entry.bundle_id == id)
        .map(|entry| entry.target_triple.as_str())
    })
}

fn channels_for_targets(inventory: &InstallationShow, targets: &[&str]) -> Vec<BuildChannels> {
  build_channels(inventory, |triple| {
    targets
      .iter()
      .any(|target| ctl_core::bundles::runnable_on(triple, target))
  })
}

fn installed_targets(inventory: &InstallationShow) -> Vec<InstalledTarget> {
  let present: BTreeSet<_> = inventory
    .builds
    .iter()
    .map(|build| &build.bundle_id)
    .collect();
  let targets = inventory
    .builds
    .iter()
    .map(|build| &build.target_triple)
    .collect::<BTreeSet<_>>();
  targets
    .into_iter()
    .map(|target| InstalledTarget {
      target_triple: target.clone(),
      builds: build_channels(inventory, |triple| triple == target),
      current_publications: inventory
        .installations
        .iter()
        .filter(|entry| {
          entry.current && &entry.target_triple == target && present.contains(&entry.bundle_id)
        })
        .filter_map(|entry| {
          let origin = entry.origin.as_ref()?;
          Some((origin.channel.clone(), origin.publication_id.clone()?))
        })
        .collect(),
    })
    .collect()
}

fn build_channels(
  inventory: &InstallationShow,
  include_target: impl Fn(&str) -> bool,
) -> Vec<BuildChannels> {
  let memberships = inventory
    .installations
    .iter()
    .filter_map(|entry| Some((&entry.bundle_id, entry.origin.as_ref()?)));
  let active = inventory
    .active
    .as_ref()
    .zip(inventory.active_origin.as_ref());
  let mut channels_by_id = BTreeMap::<_, BTreeSet<_>>::new();
  for (id, origin) in memberships.chain(active) {
    channels_by_id
      .entry(id)
      .or_default()
      .insert(&origin.channel);
  }
  let mut grouped = BTreeMap::new();
  let current: BTreeSet<_> = inventory
    .installations
    .iter()
    .filter(|entry| entry.current)
    .map(|entry| &entry.bundle_id)
    .chain(inventory.active.as_ref())
    .collect();
  for installed in inventory
    .builds
    .iter()
    .filter(|build| include_target(&build.target_triple))
  {
    let build = &installed.build;
    // Complete bundle manifests enforce one version, revision, fingerprint,
    // and dirty flag across their components. Distinct bundles can share all four.
    let key = (
      &build.version,
      &build.source_revision,
      &build.source_fingerprint,
      build.dirty,
    );
    let record = grouped.entry(key).or_insert_with(|| BuildChannels {
      build: build.clone(),
      channels: BTreeSet::new(),
      current: false,
    });
    record.current |= current.contains(&installed.bundle_id);
    if let Some(channels) = channels_by_id.get(&installed.bundle_id) {
      record.channels.extend(channels.iter().copied().cloned());
    }
  }
  grouped.into_values().collect()
}

#[derive(Serialize)]
pub(crate) struct SyncResult {
  state: &'static str,
}

#[derive(Deserialize)]
struct Checked {
  state: String,
}

#[derive(Deserialize)]
struct HostResult {
  host: String,
  result: Option<UpdateResult>,
  check: Option<Checked>,
  error: Option<String>,
}

fn host_result(bytes: &[u8], expected_host: &str) -> io::Result<HostResult> {
  let [row]: [HostResult; 1] = serde_json::from_slice(bytes)?;
  if row.host != expected_host {
    return Err(io::Error::other("ctlup reported a different update target"));
  }
  Ok(row)
}

fn sync_result(bytes: &[u8], expected_host: &str) -> io::Result<SyncResult> {
  let row = host_result(bytes, expected_host)?;
  if let Some(detail) = row.error {
    return Err(io::Error::other(detail));
  }
  let state = match (
    &row.result,
    row.check.as_ref().map(|check| check.state.as_str()),
  ) {
    (Some(_), None) => "installed",
    (None, Some("pinned")) => "pinned",
    (None, Some("current")) => "current",
    _ => return Err(io::Error::other("Unrecognized ctlup sync result")),
  };
  Ok(SyncResult { state })
}

struct SavedTarget {
  host: String,
  method: String,
  catalog: tempfile::NamedTempFile,
}

impl SavedTarget {
  fn bind(
    snapshot: &HostCatalogSnapshot,
    id: String,
    method: Option<String>,
  ) -> CommandResult<Self> {
    // The CLI accepts names and user@host aliases. Only an exact saved ID and
    // method can identify an About row. Freeze this catalog for the invocation
    // so a concurrent rename or route edit cannot redirect it.
    let host = snapshot
      .document
      .hosts
      .iter()
      .find(|host| host.host_id == id)
      .ok_or_else(|| {
        CommandErrorDto::new(
          "host_not_found",
          "This host is no longer saved. Refresh the host list.",
        )
      })?;
    let method = method
      .or_else(|| host.preferred_method_id.clone())
      .filter(|method| {
        host
          .connection_methods
          .iter()
          .any(|item| item.method_id == *method)
      })
      .ok_or_else(|| {
        CommandErrorDto::new(
          "method_not_found",
          "This SSH connection method is no longer available.",
        )
      })?;
    if host.remote_info.is_none() {
      return Err(CommandErrorDto::new(
        "remote_identity_unverified",
        "Check the saved host's identity before managing its runtime.",
      ));
    }
    let catalog = tempfile::NamedTempFile::new().map_err(CommandErrorDto::backend)?;
    serde_json::to_writer(catalog.as_file(), snapshot).map_err(CommandErrorDto::backend)?;
    Ok(Self {
      host: id,
      method,
      catalog,
    })
  }
}

async fn saved_target(
  app: tauri::AppHandle,
  host_id: Option<String>,
  method_id: Option<String>,
  cancelled: &mut watch::Receiver<bool>,
) -> CommandResult<Option<SavedTarget>> {
  match host_id {
    Some(id) => {
      let snapshot = tokio::select! {
        result = crate::workspace::load_hosts(app) => result?,
        _ = cancelled.wait_for(|stop| *stop) => return Err(CommandErrorDto::backend("The runtime operation was cancelled.")),
        () = tokio::time::sleep(Duration::from_secs(30)) => return Err(CommandErrorDto::backend("Timed out reading the saved host catalog.")),
      };
      SavedTarget::bind(&snapshot, id, method_id).map(Some)
    }
    None if method_id.is_none() => Ok(None),
    None => Err(CommandErrorDto::new(
      "invalid_target",
      "An SSH method requires a saved host.",
    )),
  }
}

fn home() -> CommandResult<std::path::PathBuf> {
  dirs::home_dir().ok_or_else(|| CommandErrorDto::backend("Home directory is unavailable."))
}

#[tauri::command]
pub(crate) async fn get_updater_version() -> CommandResult<String> {
  manager::selected_version(&home()?).await
}

fn start_operation(
  window: &str,
  attempt_id: Option<String>,
) -> CommandResult<(
  crate::component_update::OperationGuard,
  watch::Receiver<bool>,
)> {
  let id = attempt_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
  if uuid::Uuid::parse_str(&id).is_err() {
    return Err(CommandErrorDto::new(
      "invalid_attempt",
      "Invalid runtime operation ID.",
    ));
  }
  crate::component_update::register_operation(window, &id)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn about_runtime_status(
  app: tauri::AppHandle,
  window: tauri::WebviewWindow,
  host_id: Option<String>,
  method_id: Option<String>,
  attempt_id: Option<String>,
) -> CommandResult<AboutRuntimeStatus> {
  let home = home()?;
  let (_guard, mut cancelled) = start_operation(window.label(), attempt_id)?;
  let target = saved_target(app, host_id, method_id, &mut cancelled).await?;
  let inventory = if let Some(target) = target.as_ref() {
    let output = run_manager(
      &home,
      cancelled,
      ManagerAction::Status(target),
      Duration::from_secs(30),
    )
    .await?;
    serde_json::from_slice(&output).map_err(CommandErrorDto::backend)?
  } else {
    tokio::select! {
      result = tokio::task::spawn_blocking(move || ctl_client::installation::show(&home)) => result.map_err(CommandErrorDto::backend)?.map_err(CommandErrorDto::backend)?,
      _ = cancelled.wait_for(|stop| *stop) => return Err(CommandErrorDto::backend("The runtime operation was cancelled.")),
      () = tokio::time::sleep(Duration::from_secs(30)) => return Err(CommandErrorDto::backend("Timed out reading the local runtime inventory.")),
    }
  };
  let native_target = target.is_none().then(ctl_core::paths::native_target);
  AboutRuntimeStatus::from_inventory(inventory, native_target).map_err(CommandErrorDto::backend)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn sync_about_runtime(
  app: tauri::AppHandle,
  window: tauri::WebviewWindow,
  host_id: Option<String>,
  method_id: Option<String>,
  attempt_id: Option<String>,
) -> CommandResult<SyncResult> {
  let (_guard, mut cancelled) = start_operation(window.label(), attempt_id)?;
  let target = saved_target(app, host_id, method_id, &mut cancelled).await?;
  let output = run_manager(
    &home()?,
    cancelled,
    ManagerAction::Update(target.as_ref()),
    Duration::from_mins(15),
  )
  .await?;
  sync_result(
    &output,
    target.as_ref().map_or("local", |t| t.host.as_str()),
  )
  .map_err(CommandErrorDto::backend)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn fetch_about_runtime_target(
  window: tauri::WebviewWindow,
  attempt_id: String,
  target_triple: String,
  channel: String,
) -> CommandResult<()> {
  ctl_core::bundles::validate_target(&target_triple).map_err(CommandErrorDto::backend)?;
  let channel = match channel.as_str() {
    "stable" => BundleChannel::Stable,
    "nightly" => BundleChannel::Nightly,
    _ => {
      return Err(CommandErrorDto::backend(
        "Choose a stable or nightly publication.",
      ));
    }
  };
  let (_guard, cancelled) = start_operation(window.label(), Some(attempt_id))?;
  let receipt = run_manager(
    &home()?,
    cancelled,
    ManagerAction::Fetch {
      target_triple: &target_triple,
      channel,
    },
    Duration::from_mins(15),
  )
  .await?;
  verify_fetch(&receipt, &target_triple).map_err(CommandErrorDto::backend)
}

// A fetch can also advance sibling targets in the same publication cohort.
fn verify_fetch(output: &[u8], requested_target: &str) -> io::Result<()> {
  let rows: Vec<UpdateResult> = serde_json::from_slice(output)?;
  let mut targets = BTreeSet::new();
  for row in rows {
    if row.package != Package::FullBundle
      || !row.services_preserved
      || row.bundle_id.len() != 64
      || !row.bundle_id.bytes().all(|byte| byte.is_ascii_hexdigit())
      || !targets.insert(row.target_triple.clone())
    {
      return Err(io::Error::other("Invalid ctlup bundle fetch result."));
    }
    ctl_core::bundles::validate_target(&row.target_triple)?;
  }
  if !targets.contains(requested_target) {
    return Err(io::Error::other(
      "ctlup did not report storing the requested runtime target.",
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests;
