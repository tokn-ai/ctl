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
  let (ctld, ctmuxd, taskd, remote) = tokio::join!(
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
  );
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
  app.detail = Some("Versions below describe running processes and available local helpers. Opening About does not start or update them.".into());
  Ok(ComponentVersionsSnapshot {
    components: std::iter::once(app)
      .chain(ctld)
      .chain([ctmuxd, taskd])
      .chain(observations::rows(remote))
      .collect(),
  })
}

pub(crate) async fn close_window(window: &str) {
  restart::close_window(window).await;
}
