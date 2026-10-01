use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::Emitter as _;
use tokio::sync::{Mutex, oneshot};

use super::{RemoteActionOutcome, agent_version};
use crate::{
  about::observations::RemoteObservation,
  error::{CommandErrorDto, CommandResult},
  state::{AppState, AttachmentActor},
};

type Key = (String, String);
static PENDING: LazyLock<Mutex<HashMap<Key, Pending>>> = LazyLock::new(Mutex::default);

struct Pending {
  actors: Vec<Arc<AttachmentActor>>,
  result: oneshot::Sender<CommandResult<Vec<RemoteObservation>>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconnectAcknowledgement {
  action_id: String,
  results: Vec<AttachmentResult>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentResult {
  attachment_id: String,
  replacement_attachment_id: Option<String>,
  error: Option<String>,
}

#[derive(Clone, Serialize)]
struct ReconnectEvent {
  action_id: String,
  attachment_ids: Vec<String>,
}

#[tauri::command]
pub async fn ack_component_reconnect(
  window: tauri::WebviewWindow,
  state: tauri::State<'_, AppState>,
  request: ReconnectAcknowledgement,
) -> CommandResult<()> {
  acknowledge(window.label(), &state, request).await
}

async fn acknowledge(
  window: &str,
  state: &AppState,
  request: ReconnectAcknowledgement,
) -> CommandResult<()> {
  let key = (request.action_id, window.into());
  let pending = PENDING
    .lock()
    .await
    .remove(&key)
    .ok_or_else(|| failure("This reconnect action expired or belongs to another window."))?;
  let result = validate_results(
    &pending.actors,
    &state.remote_actors().await,
    request.results,
  );
  let _ = pending.result.send(result);
  Ok(())
}

fn validate_results(
  expected: &[Arc<AttachmentActor>],
  active: &[Arc<AttachmentActor>],
  results: Vec<AttachmentResult>,
) -> CommandResult<Vec<RemoteObservation>> {
  let ids: HashSet<_> = results
    .iter()
    .map(|result| result.attachment_id.as_str())
    .collect();
  if ids.len() != results.len()
    || ids.len() != expected.len()
    || !expected
      .iter()
      .all(|actor| ids.contains(actor.attachment_id.as_str()))
  {
    return Err(failure(
      "The reconnect acknowledgement did not match the requested attachments.",
    ));
  }
  let mut observations = Vec::new();
  let mut failures = Vec::new();
  let mut replacement_ids = HashSet::new();
  for result in results {
    let before = expected
      .iter()
      .find(|actor| actor.attachment_id == result.attachment_id)
      .expect("validated requested IDs");
    if let Some(error) = result.error {
      failures.push(error.chars().take(700).collect::<String>());
      continue;
    }
    let replacement = active.iter().find(|actor| {
      Some(&actor.attachment_id) == result.replacement_attachment_id.as_ref()
        && actor.window_label == before.window_label
        && actor.attachment_id != before.attachment_id
    });
    let valid = replacement.filter(|actor| {
      replacement_ids.insert(actor.attachment_id.clone())
        && !active.iter().any(|active| Arc::ptr_eq(active, before))
        && actor
          .cache_identity
          .as_ref()
          .map(|cache| (&cache.host_key, &cache.session_id))
          == before
            .cache_identity
            .as_ref()
            .map(|cache| (&cache.host_key, &cache.session_id))
        && actor
          .remote_observation
          .as_ref()
          .map(|info| &info.identity.remote_id)
          == before
            .remote_observation
            .as_ref()
            .map(|info| &info.identity.remote_id)
    });
    if let Some(observation) = valid.and_then(|actor| actor.remote_observation.clone()) {
      observations.push(observation);
    } else {
      failures.push("The replacement attachment could not be verified for the same remote environment and session.".into());
    }
  }
  if failures.is_empty() {
    Ok(observations)
  } else {
    Err(failure(format!(
      "{} attachment(s) reconnected; remaining reconnects failed: {}",
      observations.len(),
      failures.join("; ")
    )))
  }
}

pub(super) async fn execute(
  app: &tauri::AppHandle,
  actors: &[Arc<AttachmentActor>],
) -> CommandResult<RemoteActionOutcome> {
  let mut windows = BTreeMap::<String, Vec<Arc<AttachmentActor>>>::new();
  for actor in actors {
    windows
      .entry(actor.window_label.clone())
      .or_default()
      .push(Arc::clone(actor));
  }
  let action_id = uuid::Uuid::new_v4().to_string();
  let mut waits = Vec::new();
  let mut failures = Vec::new();
  for (window, actors) in windows {
    let key = (action_id.clone(), window.clone());
    let attachment_ids = actors
      .iter()
      .map(|actor| actor.attachment_id.clone())
      .collect();
    let (result, ready) = oneshot::channel();
    PENDING
      .lock()
      .await
      .insert(key.clone(), Pending { actors, result });
    let event = ReconnectEvent {
      action_id: action_id.clone(),
      attachment_ids,
    };
    if let Err(error) = app.emit_to(window, "about-reconnect-attachments", event) {
      PENDING.lock().await.remove(&key);
      failures.push(error.to_string());
    } else {
      waits.push((key, ready));
    }
  }
  let deadline = tokio::time::Instant::now() + Duration::from_secs(35);
  let mut observations = Vec::new();
  for (key, ready) in waits {
    match tokio::time::timeout_at(deadline, ready).await {
      Ok(Ok(Ok(mut observed))) => observations.append(&mut observed),
      Ok(Ok(Err(error))) => failures.push(error.message),
      _ => failures.push("A window did not finish reconnecting in time. Its existing remote sessions remain running.".into()),
    }
    PENDING.lock().await.remove(&key);
  }
  if !failures.is_empty() {
    return Err(failure(format!(
      "{} attachment(s) verified. {}",
      observations.len(),
      failures.join("; ")
    )));
  }
  // Metadata comes from the replacement actors, never from the UI's claimed version.
  let running = observations
    .first()
    .map(|observation| agent_version(&observation.identity))
    .filter(|first| {
      observations
        .iter()
        .all(|observation| agent_version(&observation.identity) == *first)
    });
  Ok(RemoteActionOutcome {
    running,
    detail: format!(
      "Reconnected and verified {} ctl-agent transport(s). Remote terminal sessions were preserved.",
      observations.len()
    ),
  })
}

fn failure(message: impl Into<String>) -> CommandErrorDto {
  CommandErrorDto::new("component_reconnect_failed", message)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn forged_or_wrong_window_acknowledgements_never_reconnect_anything() {
    let request = ReconnectAcknowledgement {
      action_id: "not-issued".into(),
      results: Vec::new(),
    };
    assert!(
      acknowledge("main", &AppState::default(), request)
        .await
        .is_err()
    );
    assert_eq!(
      PENDING.lock().await.keys().collect::<Vec<_>>(),
      Vec::<&Key>::new()
    );
  }

  #[test]
  fn extraneous_acknowledgements_cannot_claim_success() {
    let result = AttachmentResult {
      attachment_id: "unexpected".into(),
      replacement_attachment_id: Some("fake".into()),
      error: None,
    };
    assert!(validate_results(&[], &[], vec![result]).is_err());
  }
}
