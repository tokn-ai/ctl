use crate::{
  dto::TerminalCheckpointDto,
  error::{CommandErrorDto, CommandResult},
  state::AppState,
};
use rmux_client::{
  AttachmentEvent,
  cache::{CacheIdentity, CacheStore},
};
use serde::{Deserialize, Serialize};
use tauri::{State, WebviewWindow};

#[derive(Deserialize)]
pub struct CacheRequest {
  action: CacheAction,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CacheAction {
  Load {
    host_key: String,
    session_id: String,
    terminal_id: Option<String>,
  },
  Archive {
    host_key: String,
    session_id: String,
    reason: String,
  },
}
#[derive(Serialize)]
pub struct CachedPresentationDto {
  terminal_id: String,
  checkpoint: TerminalCheckpointDto,
  history: Vec<String>,
  history_gap: bool,
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CacheResponse {
  Loaded {
    cache: Option<CachedPresentationDto>,
  },
  Archived,
}

#[tauri::command]
pub async fn session_cache(
  window: WebviewWindow,
  state: State<'_, AppState>,
  request: CacheRequest,
) -> CommandResult<CacheResponse> {
  let transition = if matches!(request.action, CacheAction::Archive { .. }) {
    Some(state.window_transition(window.label()).await)
  } else {
    None
  };
  let _transition_guard = match &transition {
    Some(transition) => Some(transition.lock().await),
    None => None,
  };
  if let CacheAction::Archive {
    host_key,
    session_id,
    ..
  } = &request.action
  {
    state
      .detach_session(window.label(), host_key, session_id)
      .await?;
  }
  tokio::task::spawn_blocking(move || {
    let store = CacheStore::for_client("desktop")?;
    match request.action {
      CacheAction::Load {
        host_key,
        session_id,
        terminal_id,
      } => {
        let cache = store
          .load(&host_key, &session_id, terminal_id.as_deref())?
          .map(|cache| CachedPresentationDto {
            terminal_id: cache.terminal_id,
            checkpoint: cache.checkpoint.into(),
            history: cache.history,
            history_gap: cache.history_gap,
          });
        Ok(CacheResponse::Loaded { cache })
      }
      CacheAction::Archive {
        host_key,
        session_id,
        reason,
      } => {
        store.archive(&host_key, &session_id, &reason)?;
        Ok(CacheResponse::Archived)
      }
    }
  })
  .await
  .map_err(CommandErrorDto::backend)?
  .map_err(|error: std::io::Error| CommandErrorDto::new("local_cache_failed", error.to_string()))
}

pub async fn persist_event(
  identity: CacheIdentity,
  event: AttachmentEvent,
) -> CommandResult<AttachmentEvent> {
  tokio::task::spawn_blocking(move || {
    CacheStore::for_client("desktop")?.apply(&identity, &event)?;
    Ok(event)
  })
  .await
  .map_err(CommandErrorDto::backend)?
  .map_err(|error: std::io::Error| CommandErrorDto::new("local_cache_failed", error.to_string()))
}
