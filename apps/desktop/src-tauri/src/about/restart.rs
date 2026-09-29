use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use ctld_ipc::lifecycle::PreparedRestart;
use tokio::sync::Mutex;

use super::local::{Owner, ctld_version, owners};
use super::models::{
  PreflightRestartRequest, RestartImpact, RestartPreflight, RestartRequest, RestartResult,
};
use crate::error::{CommandErrorDto, CommandResult};

const CONFIRMATION_LIFETIME: Duration = Duration::from_mins(1);
static PREPARED: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Mutex::default);
static TRANSITION: LazyLock<Arc<Mutex<()>>> = LazyLock::new(|| Arc::new(Mutex::new(())));

struct Pending<T = PreparedRestart> {
  window: String,
  owner: Owner,
  expires_at: Instant,
  prepared: T,
}

#[tauri::command]
pub async fn preflight_restart_ctld(
  window: tauri::WebviewWindow,
  request: PreflightRestartRequest,
) -> CommandResult<RestartPreflight> {
  prepare(window.label(), &request.component_id).await
}

async fn prepare(window: &str, component_id: &str) -> CommandResult<RestartPreflight> {
  let _transition = TRANSITION
    .try_lock()
    .map_err(|_| transition_in_progress())?;
  let owner = selected_owner(component_id)?;
  let prepared = owner
    .client()
    .map_err(CommandErrorDto::backend)?
    .preflight_restart()
    .await
    .map_err(|error| lifecycle_error(&error))?;
  let before = prepared.before.as_ref().ok_or_else(|| {
    CommandErrorDto::new(
      "ctld_no_longer_running",
      "This ctld is no longer running. Refresh About to check its status.",
    )
  })?;
  let restart_token = uuid::Uuid::new_v4().to_string();
  let response = RestartPreflight {
    restart_token: restart_token.clone(),
    component_id: owner.id.clone(),
    label: owner.label.clone(),
    running: Some(ctld_version(before.binary.clone())),
    available: ctld_version(prepared.available.info.clone()),
    impact: RestartImpact {
      ssh_connections: None,
      port_forwards: None,
      vpn_connections: Some(before.active_vpn_count),
    },
  };
  let mut pending = PREPARED.lock().await;
  pending.retain(|_, pending| pending.window != window && pending.expires_at > Instant::now());
  pending.insert(
    restart_token,
    Pending {
      window: window.into(),
      owner,
      expires_at: Instant::now() + CONFIRMATION_LIFETIME,
      prepared,
    },
  );
  Ok(response)
}

#[tauri::command]
pub async fn restart_ctld(
  window: tauri::WebviewWindow,
  request: RestartRequest,
) -> CommandResult<RestartResult> {
  let transition = Arc::clone(&TRANSITION)
    .try_lock_owned()
    .map_err(|_| transition_in_progress())?;
  let pending = take_pending(window.label(), &request.restart_token).await?;
  if selected_owner(&pending.owner.id)? != pending.owner {
    return Err(CommandErrorDto::new(
      "ctld_owner_changed",
      "The selected ctld owner changed. Refresh About and confirm the restart again.",
    ));
  }
  // Once confirmed, keep cleanup and replacement running even if the caller
  // closes its window while the daemon is draining.
  tokio::spawn(async move {
    let _transition = transition;
    let outcome = pending
      .prepared
      .restart()
      .await
      .map_err(|error| lifecycle_error(&error))?;
    Ok(RestartResult {
      component_id: pending.owner.id,
      running: ctld_version(outcome.after.binary),
    })
  })
  .await
  .map_err(CommandErrorDto::backend)?
}

async fn take_pending(window: &str, token: &str) -> CommandResult<Pending> {
  let mut prepared = PREPARED.lock().await;
  take_confirmation(&mut prepared, window, token, Instant::now())
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
      "ctld_restart_confirmation_expired",
      "The restart confirmation expired. Check ctld again before restarting.",
    ));
  }
  Ok(prepared.remove(token).expect("validated pending restart"))
}

fn selected_owner(component_id: &str) -> CommandResult<Owner> {
  owners()
    .into_iter()
    .find(|owner| owner.id == component_id)
    .ok_or_else(|| {
      CommandErrorDto::new(
        "ctld_owner_unavailable",
        "This ctld owner is no longer selected by the app. Refresh About before restarting.",
      )
    })
}

fn lifecycle_error(error: &ctld_ipc::lifecycle::LifecycleError) -> CommandErrorDto {
  CommandErrorDto::new(error.code(), error.to_string())
}

fn transition_in_progress() -> CommandErrorDto {
  CommandErrorDto::new(
    "ctld_restart_in_progress",
    "Another ctld restart is already in progress.",
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

  #[test]
  fn caller_cannot_choose_an_arbitrary_endpoint_for_restart() {
    for value in ["/tmp/arbitrary-owner.sock", "ctld", "", "../../daemon.sock"] {
      assert_eq!(
        selected_owner(value).unwrap_err().code,
        "ctld_owner_unavailable"
      );
    }
  }

  #[tokio::test]
  async fn missing_confirmation_never_resolves_or_starts_a_daemon() {
    let error = take_pending("window", "not-a-token").await.err().unwrap();
    assert_eq!(error.code, "ctld_restart_confirmation_expired");
  }

  fn pending(window: &str, expires_at: Instant) -> Pending<()> {
    Pending {
      window: window.into(),
      owner: Owner {
        id: "owner".into(),
        label: "ctld".into(),
        socket: "/tmp/about-confirmation-test.sock".into(),
        executable: Ok("ctld".into()),
      },
      expires_at,
      prepared: (),
    }
  }

  #[test]
  fn confirmation_is_window_bound_single_use_and_expires_without_side_effects() {
    let now = Instant::now();
    let mut confirmations = HashMap::from([
      (
        "ready".into(),
        pending("main", now + Duration::from_mins(1)),
      ),
      ("expired".into(), pending("main", now)),
    ]);
    assert!(take_confirmation(&mut confirmations, "other", "ready", now).is_err());
    assert!(confirmations.contains_key("ready"));
    assert!(!confirmations.contains_key("expired"));
    assert!(take_confirmation(&mut confirmations, "main", "ready", now).is_ok());
    assert!(take_confirmation(&mut confirmations, "main", "ready", now).is_err());
    assert!(confirmations.is_empty());
  }
}
