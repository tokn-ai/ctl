//! Window-bound confirmations for component maintenance. Preflight is read-only.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use tauri::Emitter as _;
use tokio::sync::Mutex;

use super::local::{Owner, ctld_version, owners};
use super::models::{
  ComponentAction, ComponentActionImpact, ComponentActionPreflight, ComponentActionResult,
  ComponentVersionInfo, ExecuteComponentActionRequest, PreflightRestartRequest, ProtocolVersion,
};
use super::remote_actions::PreparedRemoteAction;
use crate::error::{CommandErrorDto, CommandResult};
use crate::state::AppState;

// Legacy ctmux control streams accept a request for thirty seconds after handshake.
const CONFIRMATION_LIFETIME: Duration = Duration::from_secs(20);
static PREPARED: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Mutex::default);
static TRANSITION: LazyLock<Arc<Mutex<()>>> = LazyLock::new(|| Arc::new(Mutex::new(())));

struct Pending<T = PreparedAction> {
  window: String,
  expires_at: Instant,
  prepared: T,
}

struct PreparedAction {
  response: ComponentActionPreflight,
  operation: Operation,
}

enum Operation {
  Ctld {
    owner: Owner,
    prepared: ctl_ipc::lifecycle::PreparedRestart,
  },
  Ctmuxd {
    socket: PathBuf,
    executable: PathBuf,
    prepared: ctmux_ipc::lifecycle::PreparedRestart,
  },
  Taskd {
    socket: PathBuf,
    executable: PathBuf,
    prepared: ctl_task_client::PreparedRestart,
  },
  Remote(PreparedRemoteAction),
}

#[tauri::command]
pub async fn preflight_component_action(
  window: tauri::WebviewWindow,
  state: tauri::State<'_, AppState>,
  request: PreflightRestartRequest,
) -> CommandResult<ComponentActionPreflight> {
  let _transition = TRANSITION
    .try_lock()
    .map_err(|_| transition_in_progress())?;
  let prepared = prepare(state.inner(), &request.component_id).await?;
  let response = prepared.response.clone();
  let token = response.action_token.clone();
  let mut pending = PREPARED.lock().await;
  pending
    .retain(|_, pending| pending.window != window.label() && pending.expires_at > Instant::now());
  pending.insert(
    token.clone(),
    Pending {
      window: window.label().into(),
      expires_at: Instant::now() + CONFIRMATION_LIFETIME,
      prepared,
    },
  );
  // Drop held owner streams even when a confirmation is abandoned and About
  // remains open. Expiration never sends a maintenance request.
  tokio::spawn(async move {
    tokio::time::sleep(CONFIRMATION_LIFETIME).await;
    PREPARED.lock().await.remove(&token);
  });
  Ok(response)
}

async fn prepare(state: &AppState, component_id: &str) -> CommandResult<PreparedAction> {
  let mut response = ComponentActionPreflight {
    action_token: uuid::Uuid::new_v4().to_string(),
    component_id: component_id.into(),
    component: "ctld",
    location: "local",
    host_id: None,
    label: String::new(),
    action: ComponentAction::Restart,
    running: None,
    available: None,
    impact: ComponentActionImpact::default(),
  };
  let operation = match component_id {
    "ctmuxd" => {
      let socket = ctmux_ipc::socket_path();
      let executable = ctmux_ipc::daemon_executable().map_err(CommandErrorDto::backend)?;
      let prepared = ctmux_ipc::lifecycle::Client::new(socket.clone())
        .with_daemon_executable(executable.clone())
        .preflight_restart()
        .await
        .map_err(|error| CommandErrorDto::new(error.code(), error.to_string()))?;
      response.component = "ctmuxd";
      response.label = "ctmuxd".into();
      response.running = Some(ctmux_version(&prepared.before));
      response.available = Some(ComponentVersionInfo::from_component(
        prepared.available.clone(),
      ));
      response.impact.description = "Ends all local terminal sessions, including sessions in other windows or apps and interactive tasks. The replacement daemon starts with its default runtime options.".into();
      Operation::Ctmuxd {
        socket,
        executable,
        prepared,
      }
    }
    "ctl-taskd" => {
      let row = super::local::taskd().await;
      let socket = ctl_task_ipc::socket_path();
      let executable = ctl_task_client::daemon_executable().map_err(CommandErrorDto::backend)?;
      let prepared = ctl_task_client::preflight_restart_at(socket.clone(), executable.clone())
        .await
        .map_err(|error| CommandErrorDto::new(error.code(), error.to_string()))?;
      response.component = "ctl-taskd";
      response.label = row.label;
      response.running = row.running;
      response.available = Some(ComponentVersionInfo::from_component(
        prepared.available.clone(),
      ));
      response.impact.description = "Restarts the local task daemon only when no tasks are running. Task definitions, history, storage location, and its terminal-daemon endpoint are preserved.".into();
      Operation::Taskd {
        socket,
        executable,
        prepared,
      }
    }
    id if id.starts_with("remote:") => {
      let prepared = super::remote_actions::prepare(state, id).await?;
      let preview = &prepared.preview;
      response.component_id.clone_from(&preview.component_id);
      response.component = preview.component;
      response.location = "remote";
      response.host_id.clone_from(&preview.host_id);
      response.label.clone_from(&preview.label);
      response.action = preview.action;
      response.running.clone_from(&preview.running);
      response.available.clone_from(&preview.available);
      response.impact.description = format!(
        "{} Active app transports: {}.",
        preview.detail, preview.attachment_count
      );
      Operation::Remote(prepared)
    }
    _ => {
      let owner = selected_owner(component_id).await?;
      let prepared = owner
        .client()
        .map_err(CommandErrorDto::backend)?
        .preflight_restart()
        .await
        .map_err(|error| CommandErrorDto::new(error.code(), error.to_string()))?;
      let before = prepared.before.as_ref().ok_or_else(|| {
        CommandErrorDto::new(
          "ctld_no_longer_running",
          "This ctld is no longer running. Refresh About to check its status.",
        )
      })?;
      response.label.clone_from(&owner.label);
      response.running = Some(ctld_version(before.binary.clone()));
      response.available = Some(ctld_version(prepared.available.info.clone()));
      response.impact.vpn_connections = Some(before.active_vpn_count);
      response.impact.description = "Stops this broker's port forwards, releases its VPN connections, and may interrupt SSH connections. Shared VPN containers remain available while another ctld keeps them alive, then expire after their heartbeat timeout. Surviving SSH connections can be reused; remote terminal sessions remain on their host.".into();
      Operation::Ctld { owner, prepared }
    }
  };
  Ok(PreparedAction {
    response,
    operation,
  })
}

#[tauri::command]
pub async fn execute_component_action(
  app: tauri::AppHandle,
  window: tauri::WebviewWindow,
  state: tauri::State<'_, AppState>,
  request: ExecuteComponentActionRequest,
) -> CommandResult<ComponentActionResult> {
  let transition = Arc::clone(&TRANSITION)
    .try_lock_owned()
    .map_err(|_| transition_in_progress())?;
  let pending = take_pending(window.label(), &request.action_token).await?;
  let state = state.inner().clone();
  // A confirmed operation finishes even if the invoking window closes.
  tokio::spawn(async move {
    let _transition = transition;
    execute(pending.prepared, &app, &state).await
  })
  .await
  .map_err(CommandErrorDto::backend)?
}

async fn execute(
  prepared: PreparedAction,
  app: &tauri::AppHandle,
  state: &AppState,
) -> CommandResult<ComponentActionResult> {
  let response = prepared.response;
  let (running, detail) = match prepared.operation {
    Operation::Ctld { owner, prepared } => {
      if selected_owner(&owner.id).await? != owner {
        return Err(selection_changed());
      }
      let outcome = prepared
        .restart()
        .await
        .map_err(|error| CommandErrorDto::new(error.code(), error.to_string()))?;
      (Some(ctld_version(outcome.after.binary)), None)
    }
    Operation::Ctmuxd {
      socket,
      executable,
      prepared,
    } => {
      let transition = state.daemon_restart_transition();
      let _transition = transition
        .try_lock()
        .map_err(|_| transition_in_progress())?;
      if socket != ctmux_ipc::socket_path()
        || executable != ctmux_ipc::daemon_executable().map_err(CommandErrorDto::backend)?
      {
        return Err(selection_changed());
      }
      let outcome = prepared.restart().await;
      if outcome.as_ref().map_or_else(
        ctmux_ipc::lifecycle::LifecycleError::may_have_stopped,
        |_| true,
      ) {
        emit_local_reset(app);
      }
      let outcome =
        outcome.map_err(|error| CommandErrorDto::new(error.code(), error.to_string()))?;
      (
        Some(ComponentVersionInfo::from_component(outcome.after)),
        Some(format!(
          "Restarted ctmuxd; {} terminal sessions ended.",
          outcome.terminated_sessions
        )),
      )
    }
    Operation::Taskd {
      socket,
      executable,
      prepared,
    } => {
      let transition = state.daemon_restart_transition();
      let _transition = transition
        .try_lock()
        .map_err(|_| transition_in_progress())?;
      if socket != ctl_task_ipc::socket_path()
        || executable != ctl_task_client::daemon_executable().map_err(CommandErrorDto::backend)?
      {
        return Err(selection_changed());
      }
      let outcome = prepared
        .restart()
        .await
        .map_err(|error| CommandErrorDto::new(error.code(), error.to_string()))?;
      (
        Some(ComponentVersionInfo::from_component(outcome.after)),
        None,
      )
    }
    Operation::Remote(prepared) => {
      let outcome = prepared.execute(app, state).await?;
      (outcome.running, Some(outcome.detail))
    }
  };
  Ok(ComponentActionResult {
    component_id: response.component_id,
    component: response.component,
    location: response.location,
    host_id: response.host_id,
    action: response.action,
    running,
    detail,
  })
}

fn emit_local_reset(app: &tauri::AppHandle) {
  let _ = app.emit(
    "about-reset-sessions",
    serde_json::json!({
      "scope": "local", "host_ids": [], "session_ids": [], "attachment_ids": [],
    }),
  );
}

fn ctmux_version(info: &ctmux_ipc::lifecycle::RunningDaemon) -> ComponentVersionInfo {
  let protocols = info
    .protocols
    .clone()
    .into_iter()
    .map(ProtocolVersion::from)
    .collect();
  match info.build.clone() {
    Some(build) => ComponentVersionInfo::from_build(build, protocols),
    None => ComponentVersionInfo {
      protocols,
      ..ComponentVersionInfo::default()
    },
  }
}

async fn take_pending(window: &str, token: &str) -> CommandResult<Pending> {
  take_confirmation(&mut *PREPARED.lock().await, window, token, Instant::now())
}

fn take_confirmation<T>(
  prepared: &mut HashMap<String, Pending<T>>,
  window: &str,
  token: &str,
  now: Instant,
) -> CommandResult<Pending<T>> {
  prepared.retain(|_, pending| pending.expires_at > now);
  if prepared
    .get(token)
    .is_none_or(|pending| pending.window != window)
  {
    return Err(CommandErrorDto::new(
      "component_action_confirmation_expired",
      "The confirmation expired. Check the component again before continuing.",
    ));
  }
  Ok(prepared.remove(token).expect("validated pending action"))
}

async fn selected_owner(component_id: &str) -> CommandResult<Owner> {
  let mut owner = owners()
    .into_iter()
    .find(|owner| owner.id == component_id)
    .ok_or_else(|| {
      CommandErrorDto::new(
        "component_owner_unavailable",
        "This component is no longer selected by the app. Refresh About before continuing.",
      )
    })?;
  owner.executable = Ok(
    crate::daemon_helper::executable()
      .await
      .map_err(CommandErrorDto::backend)?,
  );
  Ok(owner)
}

fn selection_changed() -> CommandErrorDto {
  CommandErrorDto::new(
    "component_owner_changed",
    "The selected component changed. Refresh About and confirm the action again.",
  )
}

fn transition_in_progress() -> CommandErrorDto {
  CommandErrorDto::new(
    "component_action_in_progress",
    "Another component action is already in progress.",
  )
}

pub(super) async fn close_window(window: &str) {
  PREPARED
    .lock()
    .await
    .retain(|_, pending| pending.window != window);
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(target_os = "macos")]
  #[tokio::test]
  async fn shared_helper_restart_selection_child() {
    if std::env::var("CTMUX_HELPER_TEST_MODE").as_deref() != Ok("restart") {
      return;
    }
    ctl_ipc::register_daemon_executable_provider(crate::daemon_helper::tests::provider).unwrap();
    let id = owners().remove(0).id;
    let selected = selected_owner(&id).await.unwrap();
    assert_eq!(
      selected.executable.as_ref().unwrap(),
      &crate::daemon_helper::tests::shared_executable()
    );
    let available = selected.client().unwrap().available().await.unwrap();
    assert_eq!(
      available.executable,
      crate::daemon_helper::tests::shared_executable()
    );
    assert_eq!(available.info, crate::daemon_helper::tests::binary_info());
    assert_eq!(selected_owner(&id).await.unwrap(), selected);
  }

  #[tokio::test]
  async fn caller_cannot_choose_an_arbitrary_endpoint_for_restart() {
    for value in ["/tmp/arbitrary-owner.sock", "ctld", "", "../../daemon.sock"] {
      assert_eq!(
        selected_owner(value).await.unwrap_err().code,
        "component_owner_unavailable"
      );
    }
  }

  #[tokio::test]
  async fn missing_confirmation_never_resolves_or_starts_a_daemon() {
    let error = take_pending("window", "not-a-token").await.err().unwrap();
    assert_eq!(error.code, "component_action_confirmation_expired");
  }

  #[test]
  fn confirmation_is_window_bound_single_use_and_expires_without_side_effects() {
    let now = Instant::now();
    let make = |expires_at| Pending {
      window: "main".into(),
      expires_at,
      prepared: (),
    };
    let mut confirmations = HashMap::from([
      ("ready".into(), make(now + CONFIRMATION_LIFETIME)),
      ("expired".into(), make(now)),
    ]);
    assert!(take_confirmation(&mut confirmations, "other", "ready", now).is_err());
    assert!(confirmations.contains_key("ready"));
    assert!(!confirmations.contains_key("expired"));
    assert!(take_confirmation(&mut confirmations, "main", "ready", now).is_ok());
    assert!(take_confirmation(&mut confirmations, "main", "ready", now).is_err());
    assert_eq!(
      confirmations.keys().collect::<Vec<_>>(),
      Vec::<&String>::new()
    );
  }
}
