#![cfg(unix)]

use ctl_agent::{ConnectConfig, Service};
use std::path::PathBuf;
use std::time::Duration;
use task_proto::{
  ClientMessage, ExecutionMode, PROTOCOL_VERSION, RunState, ServerMessage, TaskDefinition,
  read_frame, write_frame,
};
use tokio::io::{AsyncReadExt, DuplexStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct TestDirectory(PathBuf);

impl TestDirectory {
  fn new() -> Self {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let root = PathBuf::from("/tmp").join(format!("ctl-agent-task-{}", &token[..12]));
    std::fs::create_dir(&root).unwrap();
    Self(root)
  }

  fn config(&self) -> ConnectConfig {
    let mut config = ConnectConfig::new(self.0.join("rmux.sock"));
    config.service = Service::Task;
    config.task_socket = self.0.join("taskd.sock");
    config
  }
}

impl Drop for TestDirectory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

async fn gateway(config: &ConnectConfig) -> (DuplexStream, JoinHandle<()>) {
  let config = config.clone();
  let (mut client, gateway) = tokio::io::duplex(64 * 1024);
  let (reader, writer) = tokio::io::split(gateway);
  let relay = tokio::spawn(async move {
    ctl_agent::connect(reader, writer, &config).await.unwrap();
  });
  let mut preface = vec![0; ctl_agent::SSH_TRANSPORT_PREFACE.len()];
  timeout(TEST_TIMEOUT, client.read_exact(&mut preface))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(preface, ctl_agent::SSH_TRANSPORT_PREFACE);
  (client, relay)
}

async fn message(client: &mut DuplexStream) -> ServerMessage {
  timeout(TEST_TIMEOUT, read_frame(client))
    .await
    .unwrap()
    .unwrap()
    .expect("task daemon response")
}

async fn open(config: &ConnectConfig, request: ClientMessage) -> (DuplexStream, JoinHandle<()>) {
  let (mut client, relay) = gateway(config).await;
  write_frame(
    &mut client,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: "ctl-agent-task-test".into(),
    },
  )
  .await
  .unwrap();
  assert_eq!(
    message(&mut client).await,
    ServerMessage::HandshakeAccepted {
      protocol_version: PROTOCOL_VERSION,
    }
  );
  write_frame(&mut client, &request).await.unwrap();
  (client, relay)
}

async fn request(config: &ConnectConfig, request: ClientMessage) -> ServerMessage {
  let (mut client, relay) = open(config, request).await;
  let response = message(&mut client).await;
  drop(client);
  timeout(TEST_TIMEOUT, relay).await.unwrap().unwrap();
  response
}

async fn reject_incompatible_handshake(config: &ConnectConfig) {
  let (mut wrong_version, rejected_relay) = gateway(config).await;
  write_frame(
    &mut wrong_version,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION + 1,
      client_name: "invalid-version".into(),
    },
  )
  .await
  .unwrap();
  assert!(matches!(
    message(&mut wrong_version).await,
    ServerMessage::Error {
      code: task_proto::ErrorCode::ProtocolVersionMismatch,
      ..
    }
  ));
  // Daemon EOF also ends the relay, without waiting for client EOF.
  timeout(TEST_TIMEOUT, rejected_relay)
    .await
    .unwrap()
    .unwrap();
  drop(wrong_version);
}

#[tokio::test]
async fn task_handshake_requests_and_log_disconnect_preserve_the_running_task() {
  let directory = TestDirectory::new();
  let mut config = directory.config();
  // A running task endpoint must be reused without attempting to start either
  // daemon, even when the supplied companion paths cannot execute.
  config.taskd_bin = Some(directory.0.join("missing-taskd"));
  config.rmuxd_bin = Some(directory.0.join("missing-rmuxd"));
  let daemon = tokio::spawn(taskd::run(taskd::DaemonConfig {
    socket_path: config.task_socket.clone(),
    data_directory: directory.0.join("data"),
    rmux_socket: config.rmux_socket.clone(),
  }));
  timeout(TEST_TIMEOUT, async {
    while task_ipc::connect(&config.task_socket).await.is_err() {
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .unwrap();

  reject_incompatible_handshake(&config).await;

  let created = request(
    &config,
    ClientMessage::CreateTask {
      definition: TaskDefinition {
        name: "remote-task".into(),
        program: "sh".into(),
        arguments: vec!["-c".into(), "printf 'task-ready\\n'; exec sleep 60".into()],
        working_directory: Some(directory.0.to_string_lossy().into_owned()),
        execution_mode: ExecutionMode::Background,
      },
    },
  )
  .await;
  let ServerMessage::TaskCreated { task } = created else {
    panic!("task was not created: {created:?}");
  };
  let started = request(
    &config,
    ClientMessage::StartTask {
      task: task.task_id.clone(),
    },
  )
  .await;
  let ServerMessage::TaskStatus { task: started } = started else {
    panic!("task was not started: {started:?}");
  };
  let run = started.active_run.unwrap();
  assert_eq!(run.state, RunState::Running);

  let (mut follower, relay) = open(
    &config,
    ClientMessage::ReadLogs {
      task: task.task_id.clone(),
      after_sequence: None,
      follow: true,
    },
  )
  .await;
  assert!(matches!(
    message(&mut follower).await,
    ServerMessage::Log { .. }
  ));
  drop(follower);
  timeout(TEST_TIMEOUT, relay).await.unwrap().unwrap();

  let listed = request(&config, ClientMessage::ListTasks).await;
  let ServerMessage::TaskList { tasks } = listed else {
    panic!("task list was not returned: {listed:?}");
  };
  assert_eq!(tasks.len(), 1);
  assert_eq!(tasks[0].task_id, task.task_id);
  assert_eq!(tasks[0].active_run.as_ref().unwrap().run_id, run.run_id);
  assert_eq!(
    tasks[0].active_run.as_ref().unwrap().state,
    RunState::Running
  );

  let stopped = request(
    &config,
    ClientMessage::StopTask {
      task: task.task_id.clone(),
    },
  )
  .await;
  let ServerMessage::TaskStatus { task: stopped } = stopped else {
    panic!("task was not stopped: {stopped:?}");
  };
  assert!(stopped.active_run.is_none());
  assert_eq!(stopped.last_run.unwrap().state, RunState::Stopped);
  assert_eq!(
    request(
      &config,
      ClientMessage::RemoveTask {
        task: task.task_id.clone(),
      }
    )
    .await,
    ServerMessage::TaskRemoved {
      task_id: task.task_id
    }
  );
  daemon.abort();
  assert!(daemon.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn missing_task_endpoint_does_not_fall_back_to_rmux_or_emit_readiness() {
  let directory = TestDirectory::new();
  let config = directory.config();
  let _rmux = tokio::net::UnixListener::bind(&config.rmux_socket).unwrap();
  let (mut client, gateway) = tokio::io::duplex(64);
  let (reader, writer) = tokio::io::split(gateway);
  let error = ctl_agent::connect(reader, writer, &config)
    .await
    .unwrap_err();
  assert!(matches!(error, ctl_agent::AgentError::TaskUnavailable(_)));
  assert_eq!(client.read(&mut [0; 1]).await.unwrap(), 0);
}

#[test]
fn cli_requires_an_installed_sibling_and_ignores_taskd_bin_override() {
  let directory = TestDirectory::new();
  let executable = directory.0.join("ctl-agent");
  std::fs::copy(env!("CARGO_BIN_EXE_ctl-agent"), &executable).unwrap();
  let output = std::process::Command::new(&executable)
    .args(["connect", "--service", "task"])
    .env("TASKD_RUNTIME_DIR", &directory.0)
    .env("TASKD_BIN", &executable)
    .output()
    .unwrap();
  assert!(!output.status.success());
  assert!(output.stdout.is_empty());
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("install taskd beside ctl-agent"),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
}
