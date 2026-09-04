use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use task_ipc::{Stream, connect};
use task_proto::{
  ClientMessage, ExecutionMode, PROTOCOL_VERSION, RunState, ServerMessage, TaskDefinition,
  read_frame, write_frame,
};
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
    #[cfg(unix)]
    let temporary = PathBuf::from("/tmp");
    #[cfg(windows)]
    let temporary = std::env::temp_dir();
    let root = temporary.join(format!("taskd-test-{}", &unique[..8]));
    #[cfg(unix)]
    let socket = root.join("run/taskd.sock");
    #[cfg(windows)]
    let socket = PathBuf::from(format!(r"\\.\pipe\taskd-test-{unique}"));
    Self::launch(root, socket).await
  }

  async fn launch(root: PathBuf, socket: PathBuf) -> Self {
    let child = Command::new(env!("CARGO_BIN_EXE_taskd"))
      .arg("--socket")
      .arg(&socket)
      .arg("--data-directory")
      .arg(root.join("data"))
      .kill_on_drop(true)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::inherit())
      .spawn()
      .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
      match connect(&socket).await {
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

  async fn connect(&self) -> Stream {
    let mut stream = connect(&self.socket).await.unwrap();
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
    tokio::time::timeout(Duration::from_secs(10), read_frame(&mut stream))
      .await
      .expect("request timed out")
      .unwrap()
      .unwrap()
  }

  async fn stop(mut self) {
    self.child.kill().await.unwrap();
    let _ = std::fs::remove_dir_all(&self.root);
  }
}

#[tokio::test]
async fn background_run_streams_tail_output_and_persists_its_result() {
  let mut daemon = TestDaemon::start().await;
  let created = daemon
    .request(ClientMessage::CreateTask {
      definition: TaskDefinition {
        name: "example".into(),
        program: shell_program(),
        arguments: shell_arguments(
          "printf stdout-tail; printf stderr-tail >&2",
          "echo stdout-tail& echo stderr-tail 1>&2",
        ),
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
    match tokio::time::timeout(Duration::from_secs(10), read_frame(&mut logs))
      .await
      .expect("log follow timed out")
      .unwrap()
      .unwrap()
    {
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
  daemon.child.kill().await.unwrap();
  let daemon = TestDaemon::launch(daemon.root.clone(), daemon.socket.clone()).await;
  let restored = daemon
    .request(ClientMessage::ShowTask {
      task: "example".into(),
    })
    .await;
  let ServerMessage::TaskStatus { task } = restored else {
    panic!("{restored:?}")
  };
  assert!(task.active_run.is_none());
  assert_eq!(task.last_run.unwrap().state, RunState::Completed);
  daemon.stop().await;
}

#[tokio::test]
async fn stopping_a_background_task_records_an_intentional_stop() {
  let daemon = TestDaemon::start().await;
  let created = daemon
    .request(ClientMessage::CreateTask {
      definition: TaskDefinition {
        name: "long-running".into(),
        program: shell_program(),
        arguments: shell_arguments("sleep 30 & wait", "ping -n 30 127.0.0.1 >nul"),
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

fn shell_program() -> String {
  if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into()
}
fn shell_arguments(unix: &str, windows: &str) -> Vec<String> {
  if cfg!(windows) {
    vec!["/D".into(), "/S".into(), "/C".into(), windows.into()]
  } else {
    vec!["-c".into(), unix.into()]
  }
}

#[cfg(windows)]
#[test]
fn background_child_helper() {
  if !std::path::Path::new("helper-enabled").exists() {
    return;
  }
  let mut child = std::process::Command::new(std::env::current_exe().unwrap())
    .args(["--exact", "background_leaf_helper", "--nocapture"])
    .spawn()
    .unwrap();
  child.wait().unwrap();
}

#[cfg(windows)]
#[test]
fn background_leaf_helper() {
  use std::os::windows::fs::OpenOptionsExt;
  if !std::path::Path::new("helper-enabled").exists() {
    return;
  }
  let _file = std::fs::OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .share_mode(0)
    .open("alive")
    .unwrap();
  loop {
    std::thread::sleep(Duration::from_secs(1));
  }
}

#[cfg(windows)]
async fn running_tree(daemon: &TestDaemon) -> String {
  std::fs::write(daemon.root.join("helper-enabled"), "").unwrap();
  let response = daemon
    .request(ClientMessage::CreateTask {
      definition: TaskDefinition {
        name: "tree".into(),
        program: std::env::current_exe().unwrap().to_str().unwrap().into(),
        arguments: vec![
          "--exact".into(),
          "background_child_helper".into(),
          "--nocapture".into(),
        ],
        working_directory: Some(daemon.root.to_str().unwrap().into()),
        execution_mode: ExecutionMode::Background,
      },
    })
    .await;
  let ServerMessage::TaskCreated { task } = response else {
    panic!("{response:?}")
  };
  assert!(matches!(
    daemon
      .request(ClientMessage::StartTask {
        task: task.task_id.clone()
      })
      .await,
    ServerMessage::TaskStatus { .. }
  ));
  let deadline = Instant::now() + Duration::from_secs(10);
  while !daemon.root.join("alive").exists() {
    assert!(Instant::now() < deadline, "descendant never started");
    sleep(Duration::from_millis(20)).await;
  }
  assert!(
    std::fs::OpenOptions::new()
      .write(true)
      .open(daemon.root.join("alive"))
      .is_err()
  );
  task.task_id
}

#[cfg(windows)]
async fn assert_tree_exited(daemon: &TestDaemon) {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    if std::fs::OpenOptions::new()
      .write(true)
      .open(daemon.root.join("alive"))
      .is_ok()
    {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "descendant survived job termination"
    );
    sleep(Duration::from_millis(20)).await;
  }
}

#[cfg(windows)]
#[tokio::test]
async fn stopping_task_terminates_its_descendants() {
  let daemon = TestDaemon::start().await;
  let task = running_tree(&daemon).await;
  assert!(matches!(
    daemon.request(ClientMessage::StopTask { task }).await,
    ServerMessage::TaskStatus { .. }
  ));
  assert_tree_exited(&daemon).await;
  daemon.stop().await;
}

#[cfg(windows)]
#[tokio::test]
async fn daemon_crash_terminates_its_descendants() {
  let mut daemon = TestDaemon::start().await;
  running_tree(&daemon).await;
  daemon.child.kill().await.unwrap();
  assert_tree_exited(&daemon).await;
  let _ = std::fs::remove_dir_all(&daemon.root);
}

#[tokio::test]
async fn registration_retries_reuse_identity_and_updates_preserve_run_definition() {
  let daemon = TestDaemon::start().await;
  let task_id = Uuid::new_v4().to_string();
  let definition = TaskDefinition {
    name: "Build preview".into(),
    program: shell_program(),
    arguments: shell_arguments("exit 0", "exit /b 0"),
    working_directory: None,
    execution_mode: ExecutionMode::Background,
  };
  for _ in 0..2 {
    let response = daemon
      .request(ClientMessage::RegisterTask {
        task_id: task_id.clone(),
        definition: definition.clone(),
      })
      .await;
    assert!(matches!(response, ServerMessage::TaskCreated { task } if task.task_id == task_id));
  }
  assert!(
    matches!(daemon.request(ClientMessage::ListTasks).await, ServerMessage::TaskList { tasks } if tasks.len() == 1)
  );
  daemon
    .request(ClientMessage::StartTask {
      task: task_id.clone(),
    })
    .await;
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let response = daemon
      .request(ClientMessage::ShowTask {
        task: task_id.clone(),
      })
      .await;
    if matches!(response, ServerMessage::TaskStatus { task } if task.last_run.is_some()) {
      break;
    }
    assert!(Instant::now() < deadline);
    sleep(Duration::from_millis(20)).await;
  }
  let updated = TaskDefinition {
    arguments: shell_arguments("sleep 30 & wait", "ping -n 30 127.0.0.1 >nul"),
    ..definition.clone()
  };
  let response = daemon
    .request(ClientMessage::UpdateTask {
      task: task_id.clone(),
      definition: updated.clone(),
    })
    .await;
  let ServerMessage::TaskStatus { task } = response else {
    panic!("{response:?}")
  };
  assert_eq!(task.definition, updated);
  assert_eq!(task.last_run.unwrap().definition, Some(definition.clone()));
  daemon
    .request(ClientMessage::StartTask {
      task: task_id.clone(),
    })
    .await;
  assert!(matches!(
    daemon
      .request(ClientMessage::UpdateTask {
        task: task_id.clone(),
        definition
      })
      .await,
    ServerMessage::Error {
      code: task_proto::ErrorCode::AlreadyRunning,
      ..
    }
  ));
  daemon
    .request(ClientMessage::StopTask { task: task_id })
    .await;
  daemon.stop().await;
}

#[tokio::test]
async fn daemon_restart_refuses_active_tasks_and_releases_idle_state() {
  use task_proto::control;
  let mut daemon = TestDaemon::start().await;
  let definition = TaskDefinition {
    name: "restart-survivor".into(),
    program: shell_program(),
    arguments: shell_arguments("sleep 30 & wait", "ping -n 30 127.0.0.1 >nul"),
    working_directory: None,
    execution_mode: ExecutionMode::Background,
  };
  let ServerMessage::TaskCreated { task } = daemon
    .request(ClientMessage::CreateTask { definition })
    .await
  else {
    panic!("expected task creation");
  };
  daemon
    .request(ClientMessage::StartTask {
      task: task.task_id.clone(),
    })
    .await;
  let mut control_stream = connect(&daemon.socket).await.unwrap();
  write_frame(
    &mut control_stream,
    &control::ClientMessage::RestartDaemon {
      protocol_version: control::PROTOCOL_VERSION,
    },
  )
  .await
  .unwrap();
  let refused = read_frame::<_, control::ServerMessage>(&mut control_stream)
    .await
    .unwrap();
  assert!(
    matches!(refused, Some(control::ServerMessage::Error { message }) if message.contains("active tasks"))
  );
  let ServerMessage::TaskStatus { task: running } = daemon
    .request(ClientMessage::ShowTask {
      task: task.task_id.clone(),
    })
    .await
  else {
    panic!("expected task status")
  };
  assert!(running.active_run.is_some());
  daemon
    .request(ClientMessage::StopTask {
      task: task.task_id.clone(),
    })
    .await;

  // An idle data connection must not hold the state lock past control EOF.
  let _idle_connection = connect(&daemon.socket).await.unwrap();
  let mut control_stream = connect(&daemon.socket).await.unwrap();
  write_frame(
    &mut control_stream,
    &control::ClientMessage::RestartDaemon {
      protocol_version: control::PROTOCOL_VERSION,
    },
  )
  .await
  .unwrap();
  let accepted = read_frame::<_, control::ServerMessage>(&mut control_stream)
    .await
    .unwrap();
  assert!(
    matches!(accepted, Some(control::ServerMessage::RestartAccepted { data_directory, .. }) if data_directory == daemon.root.join("data"))
  );
  assert!(
    tokio::time::timeout(
      Duration::from_secs(5),
      read_frame::<_, control::ServerMessage>(&mut control_stream)
    )
    .await
    .unwrap()
    .unwrap()
    .is_none()
  );
  assert!(daemon.child.wait().await.unwrap().success());
  let daemon = TestDaemon::launch(daemon.root.clone(), daemon.socket.clone()).await;
  let ServerMessage::TaskStatus { task: restored } = daemon
    .request(ClientMessage::ShowTask { task: task.task_id })
    .await
  else {
    panic!("expected restored task")
  };
  assert_eq!(restored.definition, task.definition);
  assert_eq!(restored.last_run.unwrap().state, RunState::Stopped);
  daemon.stop().await;
}
