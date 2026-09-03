#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use task_proto::{
  ClientMessage, ExecutionMode, PROTOCOL_VERSION, RunState, ServerMessage, TaskDefinition,
  read_frame, write_frame,
};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

struct TestDaemon {
  child: Child,
  root: PathBuf,
  socket: PathBuf,
}

impl TestDaemon {
  async fn start() -> Self {
    let unique = Uuid::new_v4().simple().to_string();
    let root = PathBuf::from("/tmp").join(format!("taskd-test-{}", &unique[..8]));
    let socket = root.join("run/taskd.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_taskd"))
      .arg("--socket")
      .arg(&socket)
      .arg("--data-directory")
      .arg(root.join("data"))
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::inherit())
      .spawn()
      .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
      match UnixStream::connect(&socket).await {
        Ok(stream) => {
          drop(stream);
          break;
        }
        Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(20)).await,
        Err(error) => panic!("taskd did not open its socket: {error}"),
      }
    }
    Self {
      child,
      root,
      socket,
    }
  }

  async fn connect(&self) -> UnixStream {
    let mut stream = UnixStream::connect(&self.socket).await.unwrap();
    write_frame(
      &mut stream,
      &ClientMessage::Handshake {
        protocol_version: PROTOCOL_VERSION,
        client_name: "integration-test".into(),
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      read_frame(&mut stream).await.unwrap(),
      Some(ServerMessage::HandshakeAccepted { .. })
    ));
    stream
  }

  async fn request(&self, request: ClientMessage) -> ServerMessage {
    let mut stream = self.connect().await;
    write_frame(&mut stream, &request).await.unwrap();
    read_frame(&mut stream).await.unwrap().unwrap()
  }

  async fn stop(mut self) {
    self.child.kill().await.unwrap();
    let _ = std::fs::remove_dir_all(&self.root);
  }
}

#[tokio::test]
async fn background_run_streams_tail_output_and_persists_its_result() {
  let daemon = TestDaemon::start().await;
  let created = daemon
    .request(ClientMessage::CreateTask {
      definition: TaskDefinition {
        name: "example".into(),
        program: "/bin/sh".into(),
        arguments: vec![
          "-c".into(),
          "printf stdout-tail; printf stderr-tail >&2".into(),
        ],
        working_directory: None,
        execution_mode: ExecutionMode::Background,
      },
    })
    .await;
  let ServerMessage::TaskCreated { task } = created else {
    panic!("unexpected create response: {created:?}");
  };
  let started = daemon
    .request(ClientMessage::StartTask {
      task: task.task_id.clone(),
    })
    .await;
  assert!(matches!(started, ServerMessage::TaskStatus { .. }));

  let mut logs = daemon.connect().await;
  write_frame(
    &mut logs,
    &ClientMessage::ReadLogs {
      task: task.task_id.clone(),
      after_sequence: None,
      follow: true,
    },
  )
  .await
  .unwrap();
  let mut output = Vec::new();
  loop {
    match read_frame(&mut logs).await.unwrap().unwrap() {
      ServerMessage::Log { event } => output.extend(event.data),
      ServerMessage::LogsFinished => break,
      response => panic!("unexpected log response: {response:?}"),
    }
  }
  assert!(String::from_utf8_lossy(&output).contains("stdout-tail"));
  assert!(String::from_utf8_lossy(&output).contains("stderr-tail"));

  let shown = daemon
    .request(ClientMessage::ShowTask { task: task.task_id })
    .await;
  let ServerMessage::TaskStatus { task } = shown else {
    panic!("unexpected show response: {shown:?}");
  };
  assert_eq!(task.last_run.unwrap().state, RunState::Completed);
  assert!(daemon.root.join("data/state.json").is_file());
  daemon.stop().await;
}

#[tokio::test]
async fn stopping_a_background_task_records_an_intentional_stop() {
  let daemon = TestDaemon::start().await;
  let created = daemon
    .request(ClientMessage::CreateTask {
      definition: TaskDefinition {
        name: "long-running".into(),
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "sleep 30 & wait".into()],
        working_directory: None,
        execution_mode: ExecutionMode::Background,
      },
    })
    .await;
  let ServerMessage::TaskCreated { task } = created else {
    panic!("unexpected create response: {created:?}");
  };
  let started = daemon
    .request(ClientMessage::StartTask {
      task: task.task_id.clone(),
    })
    .await;
  assert!(matches!(started, ServerMessage::TaskStatus { .. }));

  let stopped = daemon
    .request(ClientMessage::StopTask { task: task.task_id })
    .await;
  let ServerMessage::TaskStatus { task } = stopped else {
    panic!("unexpected stop response: {stopped:?}");
  };
  assert!(task.active_run.is_none());
  assert_eq!(task.last_run.unwrap().state, RunState::Stopped);
  daemon.stop().await;
}
