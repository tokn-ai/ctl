//! Read-only component diagnostics and owner-bound, confirmed maintenance.

mod local;
mod models;
pub(crate) mod observations;
pub(crate) mod remote_actions;
pub(crate) mod restart;

pub use models::ComponentVersionsSnapshot;

use models::{ComponentVersionInfo, ComponentVersionRow, ProtocolVersion, VersionStatus};

#[tauri::command]
pub async fn get_component_versions(
  state: tauri::State<'_, crate::state::AppState>,
) -> crate::error::CommandResult<ComponentVersionsSnapshot> {
  let ctld_owners = local::owners();
  let (ctld, rmuxd, taskd, remote) = tokio::join!(
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
    local::rmuxd(),
    local::taskd(),
    state.remote_observations(),
  );
  let mut app = ComponentVersionRow::local("rmux", "rmux");
  app.observation = "bundled";
  app.status = VersionStatus::Current;
  app.running = Some(ComponentVersionInfo {
    version: Some(env!("CARGO_PKG_VERSION").into()),
    source_revision: (!env!("RMUX_SOURCE_REVISION").is_empty())
      .then(|| env!("RMUX_SOURCE_REVISION").into()),
    source_fingerprint: None,
    dirty: None,
    protocols: vec![
      ProtocolVersion::new("ctld", ctld_ipc::PROTOCOL_VERSION),
      ProtocolVersion::new("ctld_lifecycle", ctld_ipc::lifecycle::PROTOCOL_VERSION),
      ProtocolVersion::new("rmux", rmux_proto::PROTOCOL_VERSION),
      ProtocolVersion::new("rmux_control", rmux_ipc::LOCAL_CONTROL_PROTOCOL_VERSION),
      ProtocolVersion::new("task", task_proto::PROTOCOL_VERSION),
      ProtocolVersion::new("task_control", task_proto::control::PROTOCOL_VERSION),
      ProtocolVersion::new("ctl_identity", ctl_proto::IDENTITY_PROTOCOL_VERSION),
    ],
  });
  app.detail = Some("Versions below describe running processes and available local helpers. Opening About does not start or update them.".into());
  Ok(ComponentVersionsSnapshot {
    components: std::iter::once(app)
      .chain(ctld)
      .chain([rmuxd, taskd])
      .chain(observations::rows(remote))
      .collect(),
  })
}

pub(crate) async fn close_window(window: &str) {
  restart::close_window(window).await;
}
