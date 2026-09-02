//! Explicit inspection of app-known IDs, without enumerating daemon inventory.

use std::time::Duration;

use rmux_client::get_shell_state;
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::dto::{ConnectionTargetDto, SessionDto, ShellStateDto};
use crate::error::{CommandErrorDto, CommandResult};
use crate::transport;

#[derive(Debug, Deserialize)]
pub struct InspectKnownSessionsRequest {
  pub target: ConnectionTargetDto,
  pub session_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionInspectionDto {
  pub session_id: String,
  pub session: Option<SessionDto>,
  pub shell_state: Option<ShellStateDto>,
  pub error: Option<CommandErrorDto>,
}

#[tauri::command]
pub async fn inspect_known_sessions(
  request: InspectKnownSessionsRequest,
) -> CommandResult<Vec<SessionInspectionDto>> {
  if request.session_ids.len() > 10_000 {
    return Err(CommandErrorDto::new(
      "invalid_request",
      "Too many session references.",
    ));
  }
  let mut ids = request.session_ids.into_iter();
  let Some(first_id) = ids.next() else {
    return Ok(Vec::new());
  };
  // Fail an unreachable host once, not once per remembered session.
  let stream = transport::connect(&request.target).await?;
  let first = inspect_stream(stream, request.target.clone(), first_id).await;
  let mut results = vec![first];
  let mut tasks = JoinSet::new();
  loop {
    while tasks.len() < 4 {
      let Some(session_id) = ids.next() else {
        break;
      };
      let target = request.target.clone();
      tasks.spawn(async move {
        match transport::connect(&target).await {
          Ok(stream) => inspect_stream(stream, target, session_id).await,
          Err(error) => failed(session_id, error),
        }
      });
    }
    let Some(result) = tasks.join_next().await else {
      break;
    };
    results.push(result.map_err(CommandErrorDto::backend)?);
  }
  Ok(results)
}

async fn inspect_stream(
  stream: ctl_core::Transport,
  target: ConnectionTargetDto,
  session_id: String,
) -> SessionInspectionDto {
  let result = timeout(
    Duration::from_secs(5),
    get_shell_state(stream, &super::client_identity(), &session_id),
  )
  .await;
  match result {
    Ok(Ok(snapshot)) if snapshot.session.session_id == session_id => SessionInspectionDto {
      session_id,
      session: Some(SessionDto::new(snapshot.session, target)),
      shell_state: Some(snapshot.shell_state.into()),
      error: None,
    },
    // The wire selector also accepts names; never adopt a different session
    // whose name happens to equal a remembered ID.
    Ok(Ok(_)) => failed(
      session_id,
      CommandErrorDto::new(
        "session_not_found",
        "The remembered session no longer exists.",
      ),
    ),
    Ok(Err(error)) => failed(session_id, CommandErrorDto::client(error)),
    Err(_) => failed(
      session_id,
      CommandErrorDto::new(
        "session_inspection_timeout",
        "Session inspection timed out.",
      ),
    ),
  }
}

fn failed(session_id: String, error: CommandErrorDto) -> SessionInspectionDto {
  SessionInspectionDto {
    session_id,
    session: None,
    shell_state: None,
    error: Some(error),
  }
}
