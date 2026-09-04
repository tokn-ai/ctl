use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;
use task_proto::{DesiredState, InteractiveRun, LogEvent, LogStream, RunInfo, RunState};
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;

struct Exchange {
  request: ClientMessage,
  responses: Vec<ServerMessage>,
}

struct MockConnector {
  local: bool,
  exchanges: Mutex<VecDeque<Exchange>>,
  servers: Mutex<Vec<JoinHandle<()>>>,
  attachments: Mutex<Vec<(String, PathBuf)>>,
}

impl MockConnector {
  fn new(local: bool, exchanges: Vec<Exchange>) -> Self {
    Self {
      local,
      exchanges: Mutex::new(exchanges.into()),
      servers: Mutex::new(Vec::new()),
      attachments: Mutex::new(Vec::new()),
    }
  }

  async fn assert_complete(&self, connections: usize) {
    assert!(self.exchanges.lock().unwrap().is_empty());
    let servers = std::mem::take(&mut *self.servers.lock().unwrap());
    assert_eq!(servers.len(), connections);
    for server in servers {
      server.await.unwrap();
    }
  }
}

impl Connector for MockConnector {
  type Stream = DuplexStream;
  type Error = io::Error;

  fn connect_task(&self) -> ConnectFuture<'_, Self::Stream, Self::Error> {
    Box::pin(async {
      let exchange = self
        .exchanges
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected task connection");
      let (client, mut server) = tokio::io::duplex(16 * 1024);
      self.servers.lock().unwrap().push(tokio::spawn(async move {
        assert!(matches!(
          read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
          Some(ClientMessage::Handshake {
            protocol_version: PROTOCOL_VERSION,
            ..
          })
        ));
        write_frame(
          &mut server,
          &ServerMessage::HandshakeAccepted {
            protocol_version: PROTOCOL_VERSION,
          },
        )
        .await
        .unwrap();
        assert_eq!(
          read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
          Some(exchange.request)
        );
        for response in exchange.responses {
          write_frame(&mut server, &response).await.unwrap();
        }
      }));
      Ok(client)
    })
  }

  fn is_local_task_target(&self) -> bool {
    self.local
  }

  fn attach_interactive(&self, session: String, rmux_socket: PathBuf) -> AttachFuture<'_> {
    Box::pin(async move {
      self
        .attachments
        .lock()
        .unwrap()
        .push((session, rmux_socket));
      Ok(())
    })
  }
}

fn task_info(cwd: Option<String>) -> TaskInfo {
  TaskInfo {
    task_id: "task-42".into(),
    definition: TaskDefinition {
      name: "build".into(),
      program: "cargo".into(),
      arguments: vec!["build".into()],
      working_directory: cwd,
      execution_mode: ExecutionMode::Background,
    },
    desired_state: DesiredState::Stopped,
    active_run: None,
    last_run: None,
  }
}

fn create_command(cwd: Option<String>, start: bool) -> Command {
  Command::Create {
    name: "build".into(),
    cwd,
    mode: Mode::Background,
    start,
    from_definition: None,
    scope: ScopeArguments::default(),
    command: vec!["cargo".into(), "build".into()],
  }
}

fn create_exchange(task: &TaskInfo) -> Exchange {
  Exchange {
    request: ClientMessage::CreateTask {
      definition: task.definition.clone(),
    },
    responses: vec![ServerMessage::TaskCreated { task: task.clone() }],
  }
}

#[tokio::test]
async fn remote_create_leaves_directory_resolution_to_the_target() {
  for cwd in [None, Some("projects/build"), Some(r"C:\work\project")] {
    let cwd = cwd.map(str::to_owned);
    let task = task_info(cwd.clone());
    let connector = MockConnector::new(false, vec![create_exchange(&task)]);
    run_with_connector(create_command(cwd, false), &connector)
      .await
      .unwrap();
    connector.assert_complete(1).await;
  }
}

#[tokio::test]
async fn local_create_resolves_default_and_relative_directories_from_current_directory() {
  let current = std::env::current_dir().unwrap();
  for cwd in [None, Some("projects/build")] {
    let directory = cwd.map_or_else(|| current.clone(), |cwd| current.join(cwd));
    let task = task_info(Some(directory.to_str().unwrap().into()));
    let connector = MockConnector::new(true, vec![create_exchange(&task)]);
    run_with_connector(create_command(cwd.map(str::to_owned), false), &connector)
      .await
      .unwrap();
    connector.assert_complete(1).await;
  }
}

#[tokio::test]
async fn create_and_start_reconnects_to_the_selected_target_with_the_created_task_id() {
  let task = task_info(None);
  let connector = MockConnector::new(
    false,
    vec![
      create_exchange(&task),
      Exchange {
        request: ClientMessage::StartTask {
          task: task.task_id.clone(),
        },
        responses: vec![ServerMessage::TaskStatus { task }],
      },
    ],
  );
  run_with_connector(create_command(None, true), &connector)
    .await
    .unwrap();
  connector.assert_complete(2).await;
}

#[tokio::test]
async fn task_commands_use_the_selected_remote_connector() {
  let task = task_info(None);
  let task_id = task.task_id.clone();
  let status = ServerMessage::TaskStatus { task: task.clone() };
  let cases = vec![
    (
      Command::List,
      ClientMessage::ListTasks,
      ServerMessage::TaskList {
        tasks: vec![task.clone()],
      },
    ),
    (
      Command::Show {
        task: task_id.clone(),
      },
      ClientMessage::ShowTask {
        task: task_id.clone(),
      },
      status.clone(),
    ),
    (
      Command::Start {
        task: task_id.clone(),
      },
      ClientMessage::StartTask {
        task: task_id.clone(),
      },
      status.clone(),
    ),
    (
      Command::Stop {
        task: task_id.clone(),
      },
      ClientMessage::StopTask {
        task: task_id.clone(),
      },
      status.clone(),
    ),
    (
      Command::Restart {
        task: task_id.clone(),
      },
      ClientMessage::RestartTask {
        task: task_id.clone(),
      },
      status,
    ),
    (
      Command::Remove {
        task: task_id.clone(),
      },
      ClientMessage::RemoveTask {
        task: task_id.clone(),
      },
      ServerMessage::TaskRemoved { task_id },
    ),
  ];
  for (command, request, response) in cases {
    let connector = MockConnector::new(
      false,
      vec![Exchange {
        request,
        responses: vec![response],
      }],
    );
    run_with_connector(command, &connector).await.unwrap();
    connector.assert_complete(1).await;
  }
}

#[tokio::test]
async fn logs_follow_reads_multiple_events_from_the_selected_target() {
  let connector = MockConnector::new(
    false,
    vec![Exchange {
      request: ClientMessage::ReadLogs {
        task: "task-42".into(),
        after_sequence: Some(12),
        follow: true,
      },
      responses: vec![
        ServerMessage::Log {
          event: LogEvent {
            run_id: "run-42".into(),
            sequence: 13,
            stream: LogStream::Stdout,
            data: Vec::new(),
          },
        },
        ServerMessage::Log {
          event: LogEvent {
            run_id: "run-42".into(),
            sequence: 14,
            stream: LogStream::Stderr,
            data: Vec::new(),
          },
        },
        ServerMessage::LogsFinished,
      ],
    }],
  );
  run_with_connector(
    Command::Logs {
      task: "task-42".into(),
      after: Some(12),
      follow: true,
    },
    &connector,
  )
  .await
  .unwrap();
  connector.assert_complete(1).await;
}

#[tokio::test]
async fn interactive_attachment_is_delegated_to_the_selected_target() {
  let mut task = task_info(None);
  task.definition.execution_mode = ExecutionMode::Interactive;
  task.active_run = Some(RunInfo {
    definition: Some(task.definition.clone()),
    run_id: "run-42".into(),
    state: RunState::Running,
    started_at_ms: 1,
    ended_at_ms: None,
    exit_code: None,
    interactive: Some(InteractiveRun {
      released: false,
      rmux_socket: PathBuf::from("/remote/rmux.sock"),
      instance_id: "instance-42".into(),
      session_id: Some("session-42".into()),
    }),
  });
  let connector = MockConnector::new(
    false,
    vec![Exchange {
      request: ClientMessage::ShowTask {
        task: task.task_id.clone(),
      },
      responses: vec![ServerMessage::TaskStatus { task }],
    }],
  );
  run_with_connector(
    Command::Attach {
      task: "task-42".into(),
    },
    &connector,
  )
  .await
  .unwrap();
  connector.assert_complete(1).await;
  assert_eq!(
    *connector.attachments.lock().unwrap(),
    vec![("session-42".into(), PathBuf::from("/remote/rmux.sock"))]
  );
}

struct DefinitionDirectory(PathBuf);

impl DefinitionDirectory {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("task-cli-definition-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn scope(&self) -> ScopeArguments {
    ScopeArguments {
      global: false,
      project: Some(self.0.clone()),
    }
  }

  fn repository(&self) -> task_store::Repository {
    task_store::Repository::new(task_store::project_path(&self.0).unwrap())
  }
}

impl Drop for DefinitionDirectory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[tokio::test]
async fn create_from_definition_preserves_saved_values_and_registers_a_distinct_task() {
  let directory = DefinitionDirectory::new();
  let mut saved_definition = task_info(None).definition;
  saved_definition.execution_mode = ExecutionMode::Interactive;
  let saved = directory
    .repository()
    .save(
      &uuid::Uuid::new_v4().to_string(),
      None,
      saved_definition.clone(),
    )
    .unwrap();
  let mut registered = task_info(None);
  registered.definition = saved_definition;
  registered.definition.name = "new-instance".into();
  let connector = MockConnector::new(
    true,
    vec![
      create_exchange(&registered),
      Exchange {
        request: ClientMessage::StartTask {
          task: registered.task_id.clone(),
        },
        responses: vec![ServerMessage::TaskStatus { task: registered }],
      },
    ],
  );
  run_with_connector(
    Command::Create {
      name: "new-instance".into(),
      cwd: None,
      mode: Mode::Background,
      start: true,
      from_definition: Some(saved.definition_id.clone()),
      scope: directory.scope(),
      command: Vec::new(),
    },
    &connector,
  )
  .await
  .unwrap();
  connector.assert_complete(2).await;
  assert_eq!(
    directory.repository().load().unwrap().definitions,
    vec![saved]
  );
}

#[tokio::test]
async fn save_from_run_copies_the_historical_snapshot_without_resolving_its_directory() {
  let directory = DefinitionDirectory::new();
  let historical = task_info(Some("relative-original".into())).definition;
  let mut task = task_info(Some("/new-current-directory".into()));
  task.definition.program = "changed-program".into();
  task.last_run = Some(RunInfo {
    definition: Some(historical.clone()),
    interactive: None,
    run_id: "previous-run".into(),
    state: RunState::Completed,
    started_at_ms: 1,
    ended_at_ms: Some(2),
    exit_code: Some(0),
  });
  let connector = MockConnector::new(
    true,
    vec![Exchange {
      request: ClientMessage::ListTasks,
      responses: vec![ServerMessage::TaskList { tasks: vec![task] }],
    }],
  );
  let saved = definitions::save_record(
    SaveArguments {
      name: "saved-copy".into(),
      scope: directory.scope(),
      definition_id: None,
      expected_revision: None,
      from_run: Some("previous-run".into()),
      cwd: None,
      mode: None,
      command: Vec::new(),
    },
    &connector,
  )
  .await
  .unwrap();
  let mut expected = historical;
  expected.name = "saved-copy".into();
  assert_eq!(saved.definition, expected);
  assert_eq!(
    directory.repository().load().unwrap().definitions,
    vec![saved]
  );
  connector.assert_complete(1).await;
}

#[tokio::test]
async fn remote_create_from_local_definition_fails_before_connecting() {
  let connector = MockConnector::new(false, Vec::new());
  let error = run_with_connector(
    Command::Create {
      name: "new-instance".into(),
      cwd: None,
      mode: Mode::Background,
      start: false,
      from_definition: Some("local-build".into()),
      scope: ScopeArguments {
        global: false,
        project: Some(PathBuf::from("missing-project")),
      },
      command: Vec::new(),
    },
    &connector,
  )
  .await
  .unwrap_err();
  assert!(matches!(
    error,
    CommandError::Definition(DefinitionError::LocalOnly)
  ));
  connector.assert_complete(0).await;
}
