//! Local task lifecycle and cancellable background output. No PTY ownership.
use crate::error::{CommandErrorDto, CommandResult};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use task_proto::{ClientMessage, ServerMessage};
use tauri::{State, ipc::Channel};
use tokio::sync::{Mutex, watch};

#[derive(Clone, Default)]
pub struct TaskStreams(Arc<Mutex<HashMap<String, LogSubscription>>>);
struct LogSubscription {
  owner: String,
  cancel: watch::Sender<bool>,
  acknowledged: watch::Sender<String>,
}

impl TaskStreams {
  pub async fn close_window(&self, label: &str) {
    self.0.lock().await.retain(|_, subscription| {
      if subscription.owner == label {
        let _ = subscription.cancel.send(true);
        false
      } else {
        true
      }
    });
  }
}

fn client_error(error: task_client::ClientError) -> CommandErrorDto {
  if let task_client::ClientError::Server { code, message } = error {
    let code = serde_json::to_value(code)
      .ok()
      .and_then(|value| value.as_str().map(str::to_owned))
      .unwrap_or_else(|| "task_error".into());
    CommandErrorDto::new(code, message)
  } else {
    CommandErrorDto::backend(error)
  }
}

#[tauri::command]
pub async fn task_request(request: ClientMessage) -> CommandResult<ServerMessage> {
  if matches!(
    request,
    ClientMessage::Handshake { .. } | ClientMessage::ReadLogs { .. }
  ) {
    return Err(CommandErrorDto::new(
      "invalid_request",
      "Use the log subscription command for task output.",
    ));
  }
  task_client::request(&request).await.map_err(client_error)
}

#[derive(Deserialize)]
pub struct LogRequest {
  task_id: String,
  after_sequence: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum TaskLogEvent {
  Log {
    subscription_id: String,
    run_id: String,
    sequence: String,
    stream: task_proto::LogStream,
    data: Vec<u8>,
  },
  Finished,
  Error {
    message: String,
  },
}

#[tauri::command]
pub async fn watch_task_logs(
  window: tauri::Window,
  state: State<'_, TaskStreams>,
  request: LogRequest,
  on_event: Channel<TaskLogEvent>,
) -> CommandResult<String> {
  let after_sequence = request
    .after_sequence
    .map(|value| value.parse())
    .transpose()
    .map_err(CommandErrorDto::backend)?;
  let id = uuid::Uuid::new_v4().to_string();
  let (cancel, mut cancelled) = watch::channel(false);
  let (acknowledged, mut acknowledgement) = watch::channel(String::new());
  state.0.lock().await.insert(
    id.clone(),
    LogSubscription {
      owner: window.label().into(),
      cancel,
      acknowledged,
    },
  );
  let registry = state.inner().clone();
  let subscription_id = id.clone();
  tauri::async_runtime::spawn(async move {
    let operation = async {
      let mut stream = task_client::open(&ClientMessage::ReadLogs {
        task: request.task_id,
        after_sequence,
        follow: true,
      })
      .await
      .map_err(client_error)?;
      loop {
        let message = task_proto::read_frame(&mut stream)
          .await
          .map_err(CommandErrorDto::backend)?;
        let event = match message {
          Some(ServerMessage::Log { event }) => TaskLogEvent::Log {
            subscription_id: subscription_id.clone(),
            run_id: event.run_id,
            sequence: event.sequence.to_string(),
            stream: event.stream,
            data: event.data,
          },
          Some(ServerMessage::LogsFinished) => {
            let _ = on_event.send(TaskLogEvent::Finished);
            break;
          }
          Some(ServerMessage::Error { message, .. }) => {
            return Err(CommandErrorDto::backend(message));
          }
          _ => return Err(CommandErrorDto::backend("Task log connection closed.")),
        };
        let sequence = match &event {
          TaskLogEvent::Log { sequence, .. } => sequence.clone(),
          _ => unreachable!(),
        };
        on_event.send(event).map_err(CommandErrorDto::backend)?;
        // At most one delivery waits in the webview channel at a time.
        tokio::time::timeout(
          std::time::Duration::from_secs(30),
          acknowledgement.wait_for(|seen| seen == &sequence),
        )
        .await
        .map_err(CommandErrorDto::backend)?
        .map_err(CommandErrorDto::backend)?;
      }
      Ok::<(), CommandErrorDto>(())
    };
    tokio::select! {
      _ = cancelled.changed() => {}
      result = operation => { if let Err(error) = result { let _ = on_event.send(TaskLogEvent::Error { message: error.message }); } }
    }
    registry.0.lock().await.remove(&subscription_id);
  });
  Ok(id)
}

#[tauri::command]
pub async fn cancel_task_logs(
  window: tauri::Window,
  state: State<'_, TaskStreams>,
  subscription_id: String,
) -> CommandResult<()> {
  let mut streams = state.0.lock().await;
  if streams
    .get(&subscription_id)
    .is_some_and(|subscription| subscription.owner == window.label())
    && let Some(subscription) = streams.remove(&subscription_id)
  {
    let _ = subscription.cancel.send(true);
  }
  Ok(())
}

#[tauri::command]
pub async fn acknowledge_task_log(
  window: tauri::Window,
  state: State<'_, TaskStreams>,
  subscription_id: String,
  sequence: String,
) -> CommandResult<()> {
  if let Some(subscription) = state.0.lock().await.get(&subscription_id)
    && subscription.owner == window.label()
  {
    let _ = subscription.acknowledged.send(sequence);
  }
  Ok(())
}
