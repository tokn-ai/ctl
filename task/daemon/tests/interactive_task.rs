use std::{
  path::{Path, PathBuf},
  process::Stdio,
  time::Duration,
};

use rmux_ipc::{LocalControlClientMessage, LocalControlServerMessage, ManagedOperation};
use task_proto::{
  ClientMessage, DesiredState, ExecutionMode, RunState, ServerMessage, TaskDefinition, TaskInfo,
};
use tokio::{
  process::{Child, Command},
  task::JoinHandle,
  time::{Instant, sleep, timeout},
};
use uuid::Uuid;

struct Fixture {
  root: PathBuf,
  task_socket: PathBuf,
  rmux_socket: PathBuf,
  taskd: Child,
  rmuxd: Option<JoinHandle<Result<(), rmuxd::DaemonError>>>,
  keepalive: Option<rmux_ipc::Stream>,
}

impl Fixture {
  async fn start() -> Self {
    let id = Uuid::new_v4().simple().to_string();
    #[cfg(unix)]
    let root = PathBuf::from("/tmp").join(format!("task-pty-{}", &id[..8]));
    #[cfg(windows)]
    let root = std::env::temp_dir().join(format!("task-pty-{id}"));
    #[cfg(unix)]
    let (task_socket, rmux_socket) = (root.join("task/task.sock"), root.join("rmux/rmux.sock"));
    #[cfg(windows)]
    let (task_socket, rmux_socket) = (
      PathBuf::from(format!(r"\\.\pipe\task-pty-{id}")),
      PathBuf::from(format!(r"\\.\pipe\rmux-task-pty-{id}")),
    );
    let rmuxd = spawn_rmux(rmux_socket.clone());
    let keepalive = connect_rmux(&rmux_socket).await;
    let taskd = launch_taskd(&root, &task_socket, &rmux_socket).await;
    Self {
      root,
      task_socket,
      rmux_socket,
      taskd,
      rmuxd: Some(rmuxd),
      keepalive: Some(keepalive),
    }
  }

  async fn request(&self, request: ClientMessage) -> ServerMessage {
    timeout(Duration::from_secs(20), async {
      let mut stream = task_ipc::connect(&self.task_socket).await.unwrap();
      task_proto::write_frame(
        &mut stream,
        &ClientMessage::Handshake {
          protocol_version: task_proto::PROTOCOL_VERSION,
          client_name: "interactive-test".into(),
        },
      )
      .await
      .unwrap();
      assert!(matches!(
        task_proto::read_frame(&mut stream).await.unwrap(),
        Some(ServerMessage::HandshakeAccepted { .. })
      ));
      task_proto::write_frame(&mut stream, &request)
        .await
        .unwrap();
      task_proto::read_frame(&mut stream).await.unwrap().unwrap()
    })
    .await
    .expect("task request timed out")
  }

  async fn create(&self) -> TaskInfo {
    let response = self
      .request(ClientMessage::CreateTask {
        definition: TaskDefinition {
          name: "shell".into(),
          program: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into(),
          arguments: if cfg!(windows) {
            vec!["/D".into(), "/Q".into()]
          } else {
            vec![]
          },
          working_directory: Some(self.root.to_string_lossy().into_owned()),
          execution_mode: ExecutionMode::Interactive,
        },
      })
      .await;
    let ServerMessage::TaskCreated { task } = response else {
      panic!("{response:?}")
    };
    task
  }

  async fn status_request(&self, request: ClientMessage) -> TaskInfo {
    let response = self.request(request).await;
    let ServerMessage::TaskStatus { task } = response else {
      panic!("{response:?}")
    };
    task
  }

  async fn wait_for(&self, predicate: impl Fn(&TaskInfo) -> bool) -> TaskInfo {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
      let task = self
        .status_request(ClientMessage::ShowTask {
          task: "shell".into(),
        })
        .await;
      if predicate(&task) {
        return task;
      }
      assert!(Instant::now() < deadline, "task did not converge: {task:?}");
      sleep(Duration::from_millis(25)).await;
    }
  }

  async fn relaunch_taskd(&mut self) {
    self.taskd = launch_taskd(&self.root, &self.task_socket, &self.rmux_socket).await;
  }

  async fn close(mut self) {
    self.taskd.kill().await.unwrap();
    if let Some(daemon) = self.rmuxd.take() {
      if !daemon.is_finished() {
        let control = rmux_ipc::control_socket_path(&self.rmux_socket).unwrap();
        let stream = rmux_ipc::connect_existing_daemon(&control).await.unwrap();
        rmux_ipc::request_local_daemon_restart(stream)
          .await
          .unwrap();
      }
      self.keepalive.take();
      timeout(Duration::from_secs(15), daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    }
    std::fs::remove_dir_all(&self.root).unwrap();
  }
}

fn spawn_rmux(socket_path: PathBuf) -> JoinHandle<Result<(), rmuxd::DaemonError>> {
  tokio::spawn(rmuxd::run(rmuxd::DaemonConfig {
    socket_path,
    startup_idle_timeout: Duration::from_secs(30),
    ..Default::default()
  }))
}

async fn connect_rmux(socket: &Path) -> rmux_ipc::Stream {
  let deadline = Instant::now() + Duration::from_secs(10);
  loop {
    match rmux_ipc::connect_existing_daemon(socket).await {
      Ok(mut stream) => {
        rmux_proto::write_frame(
          &mut stream,
          &rmux_proto::ClientMessage::Handshake {
            protocol_version: rmux_proto::PROTOCOL_VERSION,
            client_name: "task-test".into(),
            client_version: "test".into(),
          },
        )
        .await
        .unwrap();
        assert!(matches!(
          rmux_message(&mut stream).await,
          rmux_proto::ServerMessage::HandshakeAccepted { .. }
        ));
        return stream;
      }
      Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(25)).await,
      Err(error) => panic!("rmuxd did not start: {error}"),
    }
  }
}

async fn launch_taskd(root: &Path, socket: &Path, rmux: &Path) -> Child {
  let child = Command::new(env!("CARGO_BIN_EXE_taskd"))
    .arg("--socket")
    .arg(socket)
    .arg("--data-directory")
    .arg(root.join("data"))
    .arg("--rmux-socket")
    .arg(rmux)
    .kill_on_drop(true)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::inherit())
    .spawn()
    .unwrap();
  let deadline = Instant::now() + Duration::from_secs(10);
  loop {
    if task_ipc::connect(socket).await.is_ok() {
      return child;
    }
    assert!(Instant::now() < deadline, "taskd did not start");
    sleep(Duration::from_millis(25)).await;
  }
}

fn session_id(task: &TaskInfo) -> &str {
  task
    .active_run
    .as_ref()
    .unwrap()
    .interactive
    .as_ref()
    .unwrap()
    .session_id
    .as_deref()
    .unwrap()
}

async fn managed(
  fixture: &Fixture,
  task: &TaskInfo,
  operation: ManagedOperation,
) -> LocalControlServerMessage {
  let run = task.active_run.as_ref().or(task.last_run.as_ref()).unwrap();
  let backend = run.interactive.as_ref().unwrap();
  let control = rmux_ipc::control_socket_path(&fixture.rmux_socket).unwrap();
  let mut stream = rmux_ipc::connect_existing_daemon(&control).await.unwrap();
  rmux_ipc::local_control_handshake(&mut stream)
    .await
    .unwrap();
  rmux_ipc::write_local_control_frame(
    &mut stream,
    &LocalControlClientMessage::ManageSession {
      expected_instance: if matches!(operation, ManagedOperation::Status) {
        None
      } else {
        Some(backend.instance_id.clone())
      },
      task_id: task.task_id.clone(),
      run_id: run.run_id.clone(),
      operation,
    },
  )
  .await
  .unwrap();
  timeout(
    Duration::from_secs(15),
    rmux_ipc::read_local_control_frame(&mut stream),
  )
  .await
  .unwrap()
  .unwrap()
  .unwrap()
}

async fn rmux_message(stream: &mut rmux_ipc::Stream) -> rmux_proto::ServerMessage {
  timeout(Duration::from_secs(15), rmux_proto::read_frame(stream))
    .await
    .unwrap()
    .unwrap()
    .unwrap()
}

async fn attach(fixture: &Fixture, session: &str) -> rmux_ipc::Stream {
  let mut stream = connect_rmux(&fixture.rmux_socket).await;
  rmux_proto::write_frame(
    &mut stream,
    &rmux_proto::ClientMessage::AttachSession {
      session: session.into(),
      resume_from: None,
      terminal_size: rmux_proto::TerminalSize::default(),
      request_input_lease: true,
      request_layout_lease: true,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: rmux_proto::DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await
  .unwrap();
  let rmux_proto::ServerMessage::Attached { checkpoint, .. } = rmux_message(&mut stream).await
  else {
    panic!("task session could not be attached");
  };
  if let Some(checkpoint) = checkpoint {
    rmux_proto::write_frame(
      &mut stream,
      &rmux_proto::ClientMessage::PresentationApplied {
        sequence: checkpoint.sequence,
      },
    )
    .await
    .unwrap();
  }
  stream
}

async fn input(stream: &mut rmux_ipc::Stream, command: &str) {
  rmux_proto::write_frame(
    stream,
    &rmux_proto::ClientMessage::Input {
      data: command.as_bytes().to_vec(),
    },
  )
  .await
  .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_task_recovers_the_same_session_and_restarts_explicitly() {
  let mut fixture = Fixture::start().await;
  fixture.create().await;
  let started = fixture
    .status_request(ClientMessage::StartTask {
      task: "shell".into(),
    })
    .await;
  fixture.keepalive.take();
  let original_id = session_id(&started).to_owned();
  let original_run = started.active_run.as_ref().unwrap().run_id.clone();
  let mut terminal = attach(&fixture, &original_id).await;
  input(&mut terminal, "echo TASK_IO_WORKS\r").await;
  let mut output = Vec::new();
  loop {
    if let rmux_proto::ServerMessage::Output {
      data, sequence_end, ..
    } = rmux_message(&mut terminal).await
    {
      output.extend(data);
      rmux_proto::write_frame(
        &mut terminal,
        &rmux_proto::ClientMessage::PresentationApplied {
          sequence: sequence_end,
        },
      )
      .await
      .unwrap();
      if String::from_utf8_lossy(&output).contains("TASK_IO_WORKS") {
        break;
      }
    }
  }
  drop(terminal);
  fixture.taskd.kill().await.unwrap();
  // Simulate a crash after rmuxd accepted creation but before taskd saved its reply.
  let path = fixture.root.join("data/state.json");
  let mut stored: serde_json::Value =
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  stored["tasks"][0]["active_run"]["state"] = "starting".into();
  stored["tasks"][0]["active_run"]["interactive"]["session_id"] = serde_json::Value::Null;
  std::fs::write(path, serde_json::to_vec(&stored).unwrap()).unwrap();
  fixture.relaunch_taskd().await;
  let recovered = fixture
    .wait_for(|task| {
      task
        .active_run
        .as_ref()
        .is_some_and(|run| run.state == RunState::Running)
    })
    .await;
  assert_eq!(session_id(&recovered), original_id);
  assert_eq!(recovered.active_run.as_ref().unwrap().run_id, original_run);
  assert!(matches!(
    fixture
      .request(ClientMessage::StartTask {
        task: "shell".into()
      })
      .await,
    ServerMessage::Error {
      code: task_proto::ErrorCode::AlreadyRunning,
      ..
    }
  ));
  assert!(matches!(
    fixture
      .request(ClientMessage::ReadLogs {
        task: "shell".into(),
        after_sequence: None,
        follow: false
      })
      .await,
    ServerMessage::Error {
      code: task_proto::ErrorCode::UnsupportedExecutionMode,
      ..
    }
  ));
  let restarted = fixture
    .status_request(ClientMessage::RestartTask {
      task: "shell".into(),
    })
    .await;
  assert_ne!(session_id(&restarted), original_id);
  assert_ne!(restarted.active_run.as_ref().unwrap().run_id, original_run);
  assert_eq!(
    restarted.last_run.as_ref().unwrap().state,
    RunState::Stopped
  );
  let stopped = fixture
    .status_request(ClientMessage::StopTask {
      task: "shell".into(),
    })
    .await;
  assert!(stopped.active_run.is_none());
  assert_eq!(stopped.last_run.as_ref().unwrap().state, RunState::Stopped);
  fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rmux_retains_exit_until_taskd_persists_it() {
  let mut fixture = Fixture::start().await;
  fixture.create().await;
  let started = fixture
    .status_request(ClientMessage::StartTask {
      task: "shell".into(),
    })
    .await;
  fixture.taskd.kill().await.unwrap();
  let mut terminal = attach(&fixture, session_id(&started)).await;
  input(
    &mut terminal,
    if cfg!(windows) {
      "exit /b 7\r"
    } else {
      "exit 7\r"
    },
  )
  .await;
  let deadline = Instant::now() + Duration::from_secs(15);
  loop {
    let status = managed(&fixture, &started, ManagedOperation::Status).await;
    if let LocalControlServerMessage::ManagedSession {
      session: Some(session),
      ..
    } = status
    {
      if !session.running {
        assert_eq!(session.exit_code, Some(7));
        break;
      }
    } else {
      panic!("{status:?}");
    }
    assert!(Instant::now() < deadline);
    sleep(Duration::from_millis(25)).await;
  }
  drop(terminal);
  fixture.keepalive.take();
  sleep(Duration::from_millis(350)).await;
  assert!(
    !fixture.rmuxd.as_ref().unwrap().is_finished(),
    "unacknowledged exit must retain rmuxd"
  );
  fixture.relaunch_taskd().await;
  let finished = fixture
    .wait_for(|task| {
      task.active_run.is_none()
        && task
          .last_run
          .as_ref()
          .is_some_and(|run| run.interactive.as_ref().unwrap().released)
    })
    .await;
  assert_eq!(finished.last_run.as_ref().unwrap().state, RunState::Failed);
  assert_eq!(finished.last_run.as_ref().unwrap().exit_code, Some(7));
  timeout(Duration::from_secs(10), fixture.rmuxd.take().unwrap())
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacing_rmuxd_fails_the_old_run_without_creating_a_new_one() {
  let mut fixture = Fixture::start().await;
  fixture.create().await;
  let started = fixture
    .status_request(ClientMessage::StartTask {
      task: "shell".into(),
    })
    .await;
  fixture.taskd.kill().await.unwrap();
  let control = rmux_ipc::control_socket_path(&fixture.rmux_socket).unwrap();
  rmux_ipc::request_local_daemon_restart(
    rmux_ipc::connect_existing_daemon(&control).await.unwrap(),
  )
  .await
  .unwrap();
  fixture.keepalive.take();
  timeout(Duration::from_secs(15), fixture.rmuxd.take().unwrap())
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  fixture.rmuxd = Some(spawn_rmux(fixture.rmux_socket.clone()));
  fixture.keepalive = Some(connect_rmux(&fixture.rmux_socket).await);
  fixture.relaunch_taskd().await;
  let finished = fixture.wait_for(|task| task.active_run.is_none()).await;
  assert_eq!(finished.desired_state, DesiredState::Stopped);
  assert_eq!(
    finished.last_run.as_ref().unwrap().run_id,
    started.active_run.as_ref().unwrap().run_id
  );
  assert_eq!(finished.last_run.as_ref().unwrap().state, RunState::Failed);
  assert_eq!(finished.last_run.as_ref().unwrap().exit_code, None);
  let stream = fixture.keepalive.as_mut().unwrap();
  rmux_proto::write_frame(stream, &rmux_proto::ClientMessage::ListSessions)
    .await
    .unwrap();
  assert!(
    matches!(rmux_message(stream).await, rmux_proto::ServerMessage::SessionList { sessions } if sessions.is_empty())
  );
  fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persisted_creation_intent_is_idempotent_and_retired_runs_stay_retired() {
  let mut fixture = Fixture::start().await;
  let mut task = fixture.create().await;
  fixture.taskd.kill().await.unwrap();
  task.desired_state = DesiredState::Running;
  task.active_run = Some(task_proto::RunInfo {
    definition: Some(task.definition.clone()),
    run_id: Uuid::new_v4().to_string(),
    state: RunState::Starting,
    started_at_ms: 1,
    ended_at_ms: None,
    exit_code: None,
    interactive: Some(task_proto::InteractiveRun {
      released: false,
      rmux_socket: fixture.rmux_socket.clone(),
      instance_id: String::new(),
      session_id: None,
    }),
  });
  let LocalControlServerMessage::ManagedSession {
    instance_id,
    session: None,
  } = managed(&fixture, &task, ManagedOperation::Status).await
  else {
    panic!("new run must not exist");
  };
  task
    .active_run
    .as_mut()
    .unwrap()
    .interactive
    .as_mut()
    .unwrap()
    .instance_id = instance_id;
  let state = serde_json::json!({"schema_version": 1, "tasks": [task]});
  std::fs::write(
    fixture.root.join("data/state.json"),
    serde_json::to_vec(&state).unwrap(),
  )
  .unwrap();
  fixture.relaunch_taskd().await;
  let running = fixture
    .wait_for(|task| {
      task
        .active_run
        .as_ref()
        .is_some_and(|run| run.state == RunState::Running)
    })
    .await;
  assert_eq!(
    running.active_run.as_ref().unwrap().run_id,
    task.active_run.as_ref().unwrap().run_id
  );
  let start = ManagedOperation::Start {
    command: rmux_proto::CommandSpec {
      program: task.definition.program,
      arguments: task.definition.arguments,
    },
    working_directory: task.definition.working_directory,
  };
  let LocalControlServerMessage::ManagedSession {
    session: Some(retried),
    ..
  } = managed(&fixture, &running, start.clone()).await
  else {
    panic!("retry must return the original session");
  };
  assert_eq!(retried.session_id, session_id(&running));
  assert!(
    matches!(
      managed(&fixture, &running, ManagedOperation::Release).await,
      LocalControlServerMessage::Error { .. }
    ),
    "a running process cannot be released"
  );
  fixture
    .status_request(ClientMessage::StopTask {
      task: "shell".into(),
    })
    .await;
  let stopped = fixture
    .wait_for(|task| {
      task
        .last_run
        .as_ref()
        .is_some_and(|run| run.interactive.as_ref().unwrap().released)
    })
    .await;
  assert!(
    matches!(
      managed(&fixture, &stopped, start).await,
      LocalControlServerMessage::Error { .. }
    ),
    "a delayed create must not resurrect a retired run"
  );
  fixture.close().await;
}
