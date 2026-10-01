use super::{State, Stream};
use ctl_task_proto::{control, write_frame};
use serde::Deserialize;
use tokio::time::{Duration, timeout};

#[derive(Deserialize)]
#[serde(untagged)]
pub enum FirstMessage {
  Control(control::ClientMessage),
  Task(ctl_task_proto::ClientMessage),
}

pub struct Request {
  pub stream: Stream,
  pub request: control::ClientMessage,
}

// The caller holds the mutation lock through acceptance and connection drain.
pub async fn accept_restart(mut request: Request, state: &State) -> Option<Stream> {
  let control::ClientMessage::RestartDaemon { protocol_version } = request.request else {
    return None;
  };
  let response = if protocol_version != control::PROTOCOL_VERSION {
    control::ServerMessage::Error {
      message: format!(
        "ctl-taskd control protocol version {} is required",
        control::PROTOCOL_VERSION
      ),
    }
  } else if state
    .tasks
    .lock()
    .await
    .values()
    .any(|task| task.active_run.is_some())
    || !state.runtimes.lock().await.is_empty()
  {
    control::ServerMessage::Error {
      message:
        "ctl-taskd has active tasks; stop them with ctl task stop before restarting ctl-taskd"
          .into(),
    }
  } else {
    control::ServerMessage::RestartAccepted {
      data_directory: state
        .persistence_path
        .parent()
        .expect("state has a directory")
        .to_owned(),
      ctmux_socket: state.ctmux_socket.clone(),
    }
  };
  let accepted = matches!(response, control::ServerMessage::RestartAccepted { .. });
  if matches!(
    timeout(
      Duration::from_secs(5),
      write_frame(&mut request.stream, &response)
    )
    .await,
    Ok(Ok(()))
  ) && accepted
  {
    Some(request.stream)
  } else {
    None
  }
}
