use super::{RequestError, State, execution_definition, now_ms};
use rmux_ipc::{
  LocalControlClientMessage, LocalControlServerMessage, ManagedOperation, ManagedSessionInfo,
};
use std::{collections::HashMap, time::Duration};
use task_proto::{DesiredState, InteractiveRun, RunInfo, RunState, TaskDefinition, TaskInfo};
use tokio::time::{Instant, timeout};
use uuid::Uuid;

#[derive(Debug)]
enum BackendError {
  Gone,
  Refused(String),
  Uncertain(String),
}

async fn exchange(
  task: &TaskInfo,
  operation: ManagedOperation,
) -> Result<(String, Option<ManagedSessionInfo>), BackendError> {
  let run = task
    .active_run
    .as_ref()
    .or(task.last_run.as_ref())
    .expect("run exists");
  let backend = run.interactive.as_ref().expect("interactive run");
  let result = timeout(Duration::from_secs(10), async {
    let path = rmux_ipc::control_socket_path(&backend.rmux_socket)
      .map_err(|error| BackendError::Uncertain(error.to_string()))?;
    let mut stream = rmux_ipc::connect_existing_daemon(&path)
      .await
      .map_err(|error| {
        if error.is_endpoint_unavailable() {
          BackendError::Gone
        } else {
          BackendError::Uncertain(error.to_string())
        }
      })?;
    let capabilities = rmux_ipc::local_control_handshake(&mut stream)
      .await
      .map_err(|error| BackendError::Uncertain(error.to_string()))?;
    if !capabilities.managed_sessions_supported {
      return Err(BackendError::Refused(
        "rmuxd does not support managed sessions; restart it with the current binary".into(),
      ));
    }
    let expected_instance = if matches!(operation, ManagedOperation::Status) {
      None
    } else {
      Some(backend.instance_id.clone())
    };
    rmux_ipc::write_local_control_frame(
      &mut stream,
      &LocalControlClientMessage::ManageSession {
        expected_instance,
        task_id: task.task_id.clone(),
        run_id: run.run_id.clone(),
        operation,
      },
    )
    .await
    .map_err(|error| BackendError::Uncertain(error.to_string()))?;
    match rmux_ipc::read_local_control_frame(&mut stream)
      .await
      .map_err(|error| BackendError::Uncertain(error.to_string()))?
    {
      Some(LocalControlServerMessage::ManagedSession {
        instance_id,
        session,
      }) => Ok((instance_id, session)),
      Some(LocalControlServerMessage::Error { message, .. }) => Err(BackendError::Refused(message)),
      _ => Err(BackendError::Uncertain(
        "invalid rmux lifecycle response".into(),
      )),
    }
  })
  .await;
  result.unwrap_or_else(|_| {
    Err(BackendError::Uncertain(
      "rmux lifecycle request timed out".into(),
    ))
  })
}

async fn connect_for_creation(socket: &std::path::Path) -> Result<rmux_ipc::Stream, RequestError> {
  timeout(Duration::from_secs(10), async {
    let mut stream = rmux_ipc::connect_or_start_daemon(socket)
      .await
      .map_err(RequestError::internal)?;
    rmux_proto::write_frame(
      &mut stream,
      &rmux_proto::ClientMessage::Handshake {
        protocol_version: rmux_proto::PROTOCOL_VERSION,
        client_name: "taskd".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
      },
    )
    .await
    .map_err(RequestError::internal)?;
    match rmux_proto::read_frame(&mut stream)
      .await
      .map_err(RequestError::internal)?
    {
      Some(rmux_proto::ServerMessage::HandshakeAccepted { .. }) => Ok(stream),
      response => Err(RequestError::internal(format!(
        "rmux handshake failed: {response:?}"
      ))),
    }
  })
  .await
  .map_err(|_| RequestError::internal("rmux connection timed out"))?
}

impl State {
  pub(super) async fn start_interactive(
    &self,
    task_id: &str,
    definition: TaskDefinition,
  ) -> Result<TaskInfo, RequestError> {
    execution_definition(&definition)?;
    // Hold a confirmed data connection while releasing the previous outcome.
    // Otherwise an idle rmuxd can exit between release and the next creation.
    // Only an explicit start may auto-start rmuxd; reconciliation never does.
    let _connection = connect_for_creation(&self.rmux_socket).await?;
    self.release_outcome(task_id).await?;
    let mut task = self.show(task_id).await?;
    task.active_run = Some(RunInfo {
      definition: Some(definition.clone()),
      interactive: Some(InteractiveRun {
        rmux_socket: self.rmux_socket.clone(),
        instance_id: String::new(),
        session_id: None,
        released: false,
      }),
      run_id: Uuid::new_v4().to_string(),
      state: RunState::Starting,
      started_at_ms: now_ms(),
      ended_at_ms: None,
      exit_code: None,
    });
    let (instance, _) = exchange(&task, ManagedOperation::Status)
      .await
      .map_err(backend_error)?;
    task
      .active_run
      .as_mut()
      .unwrap()
      .interactive
      .as_mut()
      .unwrap()
      .instance_id = instance;
    task.definition = definition;
    task.desired_state = DesiredState::Running;
    self.store_task(task).await?;
    // The durable intent and instance pin precede creation. Retrying this run
    // after taskd crashes cannot create a second process or target a new rmuxd.
    self.reconcile_one(task_id).await?;
    self.show(task_id).await
  }

  async fn store_task(&self, task: TaskInfo) -> Result<(), RequestError> {
    let mut tasks = self.tasks.lock().await;
    let previous = tasks.insert(task.task_id.clone(), task.clone());
    if let Err(error) = self.persist_blocking(&tasks) {
      if let Some(previous) = previous {
        tasks.insert(task.task_id, previous);
      }
      return Err(RequestError::internal(error));
    }
    Ok(())
  }

  async fn complete_interactive(
    &self,
    mut task: TaskInfo,
    exit_code: Option<u32>,
  ) -> Result<(), RequestError> {
    let mut run = task.active_run.take().expect("active interactive run");
    run.exit_code = exit_code.and_then(|code| i32::try_from(code).ok());
    run.ended_at_ms = Some(now_ms());
    run.state = if task.desired_state == DesiredState::Stopped {
      RunState::Stopped
    } else if exit_code == Some(0) {
      RunState::Completed
    } else {
      RunState::Failed
    };
    task.last_run = Some(run);
    task.desired_state = DesiredState::Stopped;
    self.store_task(task).await
  }

  async fn reconcile_one(&self, task_id: &str) -> Result<(), RequestError> {
    let mut task = self.show(task_id).await?;
    let Some(run) = task.active_run.as_ref() else {
      return self.release_outcome(task_id).await;
    };
    let Some(backend) = run.interactive.as_ref() else {
      return Ok(());
    };
    let instance = backend.instance_id.clone();
    let needs_creation = backend.session_id.is_none();
    let response = exchange(&task, ManagedOperation::Status).await;
    let mut session = match response {
      Ok((observed, _)) if observed != instance => {
        return self.complete_interactive(task, None).await;
      }
      Ok((_, session)) => session,
      Err(BackendError::Gone) => return self.complete_interactive(task, None).await,
      Err(error) => {
        task.active_run.as_mut().unwrap().state = RunState::Unknown;
        self.store_task(task).await?;
        return Err(backend_error(error));
      }
    };
    let operation = if task.desired_state == DesiredState::Stopped {
      Some(ManagedOperation::Stop)
    } else if session.is_none() && needs_creation {
      let definition = execution_definition(&task.definition)?;
      Some(ManagedOperation::Start {
        command: rmux_proto::CommandSpec {
          program: definition.program,
          arguments: definition.arguments,
        },
        working_directory: definition.working_directory,
      })
    } else {
      None
    };
    if let Some(operation) = operation {
      let creating = matches!(operation, ManagedOperation::Start { .. });
      match exchange(&task, operation).await {
        Ok((_, observed)) => session = observed,
        Err(BackendError::Gone) => return self.complete_interactive(task, None).await,
        Err(BackendError::Refused(message)) if creating => {
          self.complete_interactive(task, None).await?;
          return Err(RequestError::internal(message));
        }
        Err(error) => {
          task.active_run.as_mut().unwrap().state = RunState::Unknown;
          self.store_task(task).await?;
          return Err(backend_error(error));
        }
      }
    }
    match session {
      Some(session) if session.running => {
        let run = task.active_run.as_mut().unwrap();
        if run.state != RunState::Running
          || run.interactive.as_ref().unwrap().session_id.as_ref() != Some(&session.session_id)
        {
          run.state = RunState::Running;
          run.interactive.as_mut().unwrap().session_id = Some(session.session_id);
          self.store_task(task).await?;
        }
      }
      session => {
        if let Some(session) = &session {
          task
            .active_run
            .as_mut()
            .unwrap()
            .interactive
            .as_mut()
            .unwrap()
            .session_id = Some(session.session_id.clone());
        }
        self
          .complete_interactive(task, session.and_then(|session| session.exit_code))
          .await?;
      }
    }
    Ok(())
  }

  pub(super) async fn release_outcome(&self, task_id: &str) -> Result<(), RequestError> {
    let mut task = self.show(task_id).await?;
    if task.active_run.is_some() {
      return Ok(());
    }
    let Some(backend) = task
      .last_run
      .as_ref()
      .and_then(|run| run.interactive.as_ref())
    else {
      return Ok(());
    };
    if backend.released {
      return Ok(());
    }
    match exchange(&task, ManagedOperation::Status).await {
      Ok((instance, _)) if instance == backend.instance_id => {
        exchange(&task, ManagedOperation::Release)
          .await
          .map_err(backend_error)?;
      }
      Ok(_) | Err(BackendError::Gone) => {}
      Err(error) => return Err(backend_error(error)),
    }
    task
      .last_run
      .as_mut()
      .unwrap()
      .interactive
      .as_mut()
      .unwrap()
      .released = true;
    self.store_task(task).await
  }

  pub(super) async fn reconcile_interactive(&self) {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut failures = HashMap::new();
    loop {
      interval.tick().await;
      for task in self.list().await {
        let _mutation = self.mutations.lock().await;
        // A remove request can win the mutation lock after the list snapshot.
        if !self.tasks.lock().await.contains_key(&task.task_id) {
          failures.remove(&task.task_id);
          continue;
        }
        match self.reconcile_one(&task.task_id).await {
          Ok(()) => {
            failures.remove(&task.task_id);
          }
          Err(error) => {
            if failures.get(&task.task_id) != Some(&error.message) {
              eprintln!("taskd: task {}: {}", task.task_id, error.message);
            }
            failures.insert(task.task_id, error.message);
          }
        }
      }
    }
  }

  pub(super) async fn stop_interactive(&self, task_id: &str) -> Result<TaskInfo, RequestError> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
      self.reconcile_one(task_id).await?;
      let task = self.show(task_id).await?;
      if task.active_run.is_none() {
        return Ok(task);
      }
      if Instant::now() >= deadline {
        return Err(RequestError::internal("rmux session did not stop"));
      }
      tokio::time::sleep(Duration::from_millis(25)).await;
    }
  }
}

fn backend_error(error: BackendError) -> RequestError {
  RequestError::internal(match error {
    BackendError::Gone => "rmuxd is unavailable".into(),
    BackendError::Refused(message) | BackendError::Uncertain(message) => message,
  })
}
