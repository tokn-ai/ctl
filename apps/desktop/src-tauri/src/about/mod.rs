//! Read-only component diagnostics and owner-bound, confirmed maintenance.

mod inventory;
mod local;
mod models;
pub(crate) mod observations;
pub(crate) mod remote_actions;
pub(crate) mod restart;

pub use models::ComponentVersionsSnapshot;

use models::{ComponentVersionInfo, ComponentVersionRow, ProtocolVersion, VersionStatus};

#[tauri::command(rename_all = "snake_case")]
pub async fn get_component_versions(
  app: tauri::AppHandle,
  state: tauri::State<'_, crate::state::AppState>,
  host_id: Option<String>,
) -> crate::error::CommandResult<ComponentVersionsSnapshot> {
  let ctld_owners = local::owners();
  let (ctld, ctmuxd, taskd, remote, saved) = tokio::join!(
    async {
      let mut tasks = tokio::task::JoinSet::new();
      for owner in ctld_owners {
        tasks.spawn(local::ctld(owner));
      }
      let mut rows = Vec::new();
      while let Some(result) = tasks.join_next().await {
        match result {
          Ok(row) => rows.push(row),
          Err(error) => {
            let mut row = ComponentVersionRow::local("ctld", "ctld");
            row.status = VersionStatus::Unavailable;
            row.error = Some(error.to_string());
            rows.push(row);
          }
        }
      }
      rows.sort_by(|left, right| left.label.cmp(&right.label));
      rows
    },
    local::ctmuxd(),
    local::taskd(),
    state.remote_observations(),
    inventory::rows(&app, host_id.as_deref()),
  );
  let saved = saved?;
  let mut active = observations::rows(remote);
  for row in &mut active {
    row.observation = "last_observed";
    row.installed = saved
      .iter()
      .find(|saved| saved.host_id == row.host_id && saved.component == row.component)
      .and_then(|saved| saved.installed.clone());
    row.compare_installed();
  }
  active.retain(|row| {
    row.component == "ctl_agent"
      || !saved.iter().any(|saved| {
        saved.host_id == row.host_id
          && saved.component == row.component
          && matches!(saved.observation, "running" | "legacy")
          && saved.status != VersionStatus::Unavailable
      })
  });
  let mut app = ComponentVersionRow::local("ctmux", "ctmux");
  app.observation = "bundled";
  app.status = VersionStatus::Current;
  app.running = Some(ComponentVersionInfo {
    version: Some(env!("CARGO_PKG_VERSION").into()),
    source_revision: (!env!("CTMUX_SOURCE_REVISION").is_empty())
      .then(|| env!("CTMUX_SOURCE_REVISION").into()),
    source_fingerprint: None,
    dirty: None,
    protocols: ctl_ipc::lifecycle::DaemonBinaryInfo::current()
      .protocols
      .into_iter()
      .chain([
        ctmux_proto::protocol_info(),
        ctmux_ipc::local_control_protocol_info(),
        ctl_task_proto::protocol_info(),
        ctl_task_proto::control::protocol_info(),
      ])
      .chain(ctl_proto::agent_protocols())
      .map(ProtocolVersion::from)
      .collect(),
  });
  app.detail = Some("Opening Components inspects existing owners and installed helpers without starting or updating services.".into());
  Ok(ComponentVersionsSnapshot {
    components: std::iter::once(app)
      .chain(ctld)
      .chain([ctmuxd, taskd])
      .chain(saved)
      .chain(active)
      .collect(),
  })
}

pub(crate) async fn close_window(window: &str) {
  restart::close_window(window).await;
}
