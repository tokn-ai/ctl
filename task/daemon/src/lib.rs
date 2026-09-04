mod interactive;
mod process;

#[cfg(windows)]
use interprocess::local_socket::traits::tokio::Listener as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use task_ipc::Stream;
pub use task_ipc::socket_path;
use task_proto::{
  ClientMessage, DesiredState, ErrorCode, ExecutionMode, LogEvent, LogStream, PROTOCOL_VERSION,
  RunInfo, RunState, ServerMessage, TaskDefinition, TaskInfo, read_frame, write_frame,
};
use thiserror::Error;
use tokio::io::AsyncReadExt;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::time::{Instant, timeout};
use uuid::Uuid;

const STATE_SCHEMA_VERSION: u16 = 1;
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LOG_BYTES_PER_RUN: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
  pub socket_path: PathBuf,
  pub data_directory: PathBuf,
  pub rmux_socket: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredState {
  schema_version: u16,
  tasks: Vec<TaskInfo>,
}

#[derive(Debug, Clone)]
enum Activity {
  Log { task_id: String, event: LogEvent },
  Finished { task_id: String, run_id: String },
}

#[derive(Debug, Clone)]
struct RuntimeHandle {
  stop: mpsc::Sender<()>,
}

struct State {
  mutations: Mutex<()>,
  tasks: Mutex<BTreeMap<String, TaskInfo>>,
  runtimes: Mutex<HashMap<String, RuntimeHandle>>,
  logs: Mutex<HashMap<String, Vec<LogEvent>>>,
  activity: broadcast::Sender<Activity>,
  persistence_path: PathBuf,
  rmux_socket: PathBuf,
  _state_lock: fs::File,
}

impl State {
  fn load(data_directory: &Path, rmux_socket: PathBuf) -> Result<Self, DaemonError> {
    prepare_data_directory(data_directory)?;
    let state_lock = fs::OpenOptions::new()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(data_directory.join("state.lock"))
      .map_err(DaemonError::WriteState)?;
    state_lock
      .try_lock()
      .map_err(|error| DaemonError::WriteState(error.into()))?;
    let state_path = data_directory.join("state.json");
    let mut tasks = if state_path.exists() {
      let bytes = fs::read(&state_path).map_err(DaemonError::ReadState)?;
      let stored: StoredState = serde_json::from_slice(&bytes).map_err(DaemonError::ParseState)?;
      if stored.schema_version != STATE_SCHEMA_VERSION {
        return Err(DaemonError::UnsupportedStateVersion(stored.schema_version));
      }
      stored
        .tasks
        .into_iter()
        .map(|task| (task.task_id.clone(), task))
        .collect()
    } else {
      BTreeMap::new()
    };

    let now = now_ms();
    for task in tasks.values_mut() {
      if let Some(run) = task.active_run.as_mut()
        && run.interactive.is_some()
      {
        run.state = if run.state == RunState::Starting {
          RunState::Starting
        } else {
          RunState::Unknown
        };
        continue;
      }
      if let Some(mut run) = task.active_run.take() {
        run.state = RunState::Failed;
        run.ended_at_ms = Some(now);
        run.exit_code = None;
        task.last_run = Some(run);
        task.desired_state = DesiredState::Stopped;
      }
    }
    let (activity, _) = broadcast::channel(1024);
    let state = Self {
      mutations: Mutex::new(()),
      tasks: Mutex::new(tasks),
      runtimes: Mutex::new(HashMap::new()),
      logs: Mutex::new(HashMap::new()),
      activity,
      persistence_path: state_path,
      rmux_socket,
      _state_lock: state_lock,
    };
    state.persist_blocking(
      &state
        .tasks
        .try_lock()
        .expect("new task state lock is uncontended"),
    )?;
    Ok(state)
  }

  fn persist_blocking(&self, tasks: &BTreeMap<String, TaskInfo>) -> Result<(), DaemonError> {
    let stored = StoredState {
      schema_version: STATE_SCHEMA_VERSION,
      tasks: tasks.values().cloned().collect(),
    };
    let bytes = serde_json::to_vec_pretty(&stored).map_err(DaemonError::SerializeState)?;
    let temporary = self.persistence_path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(DaemonError::WriteState)?;
    #[cfg(unix)]
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
      .map_err(DaemonError::WriteState)?;
    fs::rename(&temporary, &self.persistence_path).map_err(DaemonError::WriteState)
  }

  async fn create(&self, definition: TaskDefinition) -> Result<TaskInfo, RequestError> {
    validate_definition(&definition)?;
    let mut tasks = self.tasks.lock().await;
    if tasks
      .values()
      .any(|task| task.definition.name == definition.name)
    {
      return Err(RequestError::new(
        ErrorCode::NameConflict,
        format!("a task named {:?} already exists", definition.name),
      ));
    }
    let task = TaskInfo {
      task_id: Uuid::new_v4().to_string(),
      definition,
      desired_state: DesiredState::Stopped,
      active_run: None,
      last_run: None,
    };
    tasks.insert(task.task_id.clone(), task.clone());
    self
      .persist_blocking(&tasks)
      .map_err(RequestError::internal)?;
    Ok(task)
  }

  async fn list(&self) -> Vec<TaskInfo> {
    let mut tasks: Vec<_> = self.tasks.lock().await.values().cloned().collect();
    tasks.sort_by(|left, right| left.definition.name.cmp(&right.definition.name));
    tasks
  }

  async fn show(&self, selector: &str) -> Result<TaskInfo, RequestError> {
    let tasks = self.tasks.lock().await;
    resolve_task(&tasks, selector).cloned()
  }

  async fn start(self: &Arc<Self>, selector: &str) -> Result<TaskInfo, RequestError> {
    let (task_id, definition) = {
      let tasks = self.tasks.lock().await;
      let task = resolve_task(&tasks, selector)?;
      if task.active_run.is_some() {
        return Err(RequestError::new(
          ErrorCode::AlreadyRunning,
          format!("task {:?} is already running", task.definition.name),
        ));
      }
      (task.task_id.clone(), task.definition.clone())
    };

    if definition.execution_mode == ExecutionMode::Interactive {
      return self.start_interactive(&task_id, definition).await;
    }
    let mut child = process::spawn(&definition).map_err(|error| {
      RequestError::new(
        ErrorCode::InvalidDefinition,
        format!("could not start {:?}: {error}", definition.program),
      )
    })?;
    let stdout = child.stdout();
    let stderr = child.stderr();
    let run_id = Uuid::new_v4().to_string();
    let run = RunInfo {
      interactive: None,
      run_id: run_id.clone(),
      state: RunState::Running,
      started_at_ms: now_ms(),
      ended_at_ms: None,
      exit_code: None,
    };

    {
      let mut tasks = self.tasks.lock().await;
      let task = tasks
        .get_mut(&task_id)
        .expect("task remains registered while starting");
      task.desired_state = DesiredState::Running;
      task.active_run = Some(run);
      if let Err(error) = self.persist_blocking(&tasks) {
        let _ = child.start_kill();
        return Err(RequestError::internal(error));
      }
    }

    let (stop, mut stop_receiver) = mpsc::channel(1);
    self
      .runtimes
      .lock()
      .await
      .insert(task_id.clone(), RuntimeHandle { stop });
    self.logs.lock().await.insert(run_id.clone(), Vec::new());

    let mut log_readers = Vec::new();
    if let Some(stdout) = stdout {
      log_readers.push(spawn_log_reader(
        Arc::clone(self),
        task_id.clone(),
        run_id.clone(),
        LogStream::Stdout,
        stdout,
      ));
    }
    if let Some(stderr) = stderr {
      log_readers.push(spawn_log_reader(
        Arc::clone(self),
        task_id.clone(),
        run_id.clone(),
        LogStream::Stderr,
        stderr,
      ));
    }

    let state = Arc::clone(self);
    let finish_task_id = task_id.clone();
    tokio::spawn(async move {
      let (stopped, status) = tokio::select! {
        status = child.wait() => (false, status),
        _ = stop_receiver.recv() => {
          (true, child.terminate().await)
        }
      };
      child.finish();
      let exit_code = status.ok().and_then(|status| status.code());
      for reader in log_readers {
        let _ = reader.await;
      }
      state
        .finish_run(&finish_task_id, &run_id, stopped, exit_code)
        .await;
    });

    self.show(&task_id).await
  }

  async fn finish_run(&self, task_id: &str, run_id: &str, stopped: bool, exit_code: Option<i32>) {
    self.runtimes.lock().await.remove(task_id);
    let mut tasks = self.tasks.lock().await;
    if let Some(task) = tasks.get_mut(task_id)
      && task.active_run.as_ref().map(|run| run.run_id.as_str()) == Some(run_id)
    {
      let mut run = task.active_run.take().expect("active run was checked");
      run.ended_at_ms = Some(now_ms());
      run.exit_code = exit_code;
      run.state = if stopped {
        RunState::Stopped
      } else if exit_code == Some(0) {
        RunState::Completed
      } else {
        RunState::Failed
      };
      task.last_run = Some(run);
      task.desired_state = DesiredState::Stopped;
      let _ = self.persist_blocking(&tasks);
    }
    drop(tasks);
    let _ = self.activity.send(Activity::Finished {
      task_id: task_id.into(),
      run_id: run_id.into(),
    });
  }

  async fn stop(&self, selector: &str) -> Result<TaskInfo, RequestError> {
    let task_id = {
      let mut tasks = self.tasks.lock().await;
      let task = resolve_task_mut(&mut tasks, selector)?;
      if task.active_run.is_none() {
        return Err(RequestError::new(
          ErrorCode::NotRunning,
          format!("task {:?} is not running", task.definition.name),
        ));
      }
      let previous = task.desired_state;
      task.desired_state = DesiredState::Stopped;
      let task_id = task.task_id.clone();
      if let Err(error) = self.persist_blocking(&tasks) {
        tasks.get_mut(&task_id).unwrap().desired_state = previous;
        return Err(RequestError::internal(error));
      }
      task_id
    };
    if self
      .show(&task_id)
      .await?
      .active_run
      .as_ref()
      .is_some_and(|run| run.interactive.is_some())
    {
      return self.stop_interactive(&task_id).await;
    }
    let mut activity = self.activity.subscribe();
    let runtime = self.runtimes.lock().await.get(&task_id).cloned();
    let Some(runtime) = runtime else {
      let task = self.show(&task_id).await?;
      return if task.active_run.is_none() {
        Ok(task)
      } else {
        Err(RequestError::new(
          ErrorCode::Internal,
          "running task has no process handle",
        ))
      };
    };
    if runtime.stop.send(()).await.is_err() {
      let task = self.show(&task_id).await?;
      if task.active_run.is_none() {
        return Ok(task);
      }
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
      if self.show(&task_id).await?.active_run.is_none() {
        return self.show(&task_id).await;
      }
      let remaining = deadline.saturating_duration_since(Instant::now());
      if remaining.is_zero() {
        return Err(RequestError::new(
          ErrorCode::Internal,
          "task process did not stop within five seconds",
        ));
      }
      match timeout(remaining, activity.recv()).await {
        Ok(Ok(Activity::Finished { task_id: id, .. })) if id == task_id => {
          return self.show(&task_id).await;
        }
        Ok(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
        Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => {
          return Err(RequestError::new(
            ErrorCode::Internal,
            "task stop notification failed",
          ));
        }
      }
    }
  }

  async fn restart(self: &Arc<Self>, selector: &str) -> Result<TaskInfo, RequestError> {
    let task = self.show(selector).await?;
    if task.active_run.is_some() {
      self.stop(&task.task_id).await?;
    }
    self.start(&task.task_id).await
  }

  async fn remove(&self, selector: &str) -> Result<String, RequestError> {
    let task = self.show(selector).await?;
    self.release_outcome(&task.task_id).await?;
    let mut tasks = self.tasks.lock().await;
    let task = resolve_task(&tasks, selector)?;
    if task.active_run.is_some() {
      return Err(RequestError::new(
        ErrorCode::AlreadyRunning,
        "stop the task before removing it",
      ));
    }
    let task_id = task.task_id.clone();
    tasks.remove(&task_id);
    self
      .persist_blocking(&tasks)
      .map_err(RequestError::internal)?;
    Ok(task_id)
  }

  async fn send_logs(
    &self,
    stream: &mut Stream,
    selector: &str,
    after_sequence: Option<u64>,
    follow: bool,
  ) -> Result<(), RequestError> {
    let mut activity = self.activity.subscribe();
    let task = self.show(selector).await?;
    if task.definition.execution_mode == ExecutionMode::Interactive {
      return Err(RequestError::new(
        ErrorCode::UnsupportedExecutionMode,
        "interactive output belongs to rmuxd; use ctl task attach",
      ));
    }
    let run = task.active_run.as_ref().or(task.last_run.as_ref());
    let Some(run) = run else {
      write_frame(stream, &ServerMessage::LogsFinished)
        .await
        .map_err(RequestError::internal)?;
      return Ok(());
    };
    let run_id = run.run_id.clone();
    let is_active = task.active_run.is_some();
    let mut cursor = after_sequence;
    let existing = self
      .logs
      .lock()
      .await
      .get(&run_id)
      .cloned()
      .unwrap_or_default();
    for event in existing {
      if cursor.is_none_or(|sequence| event.sequence > sequence) {
        cursor = Some(event.sequence);
        write_frame(stream, &ServerMessage::Log { event })
          .await
          .map_err(RequestError::internal)?;
      }
    }
    if !follow || !is_active {
      write_frame(stream, &ServerMessage::LogsFinished)
        .await
        .map_err(RequestError::internal)?;
      return Ok(());
    }
    loop {
      match activity.recv().await {
        Ok(Activity::Log { task_id, event })
          if task_id == task.task_id
            && event.run_id == run_id
            && cursor.is_none_or(|sequence| event.sequence > sequence) =>
        {
          cursor = Some(event.sequence);
          write_frame(stream, &ServerMessage::Log { event })
            .await
            .map_err(RequestError::internal)?;
        }
        Ok(Activity::Finished {
          task_id,
          run_id: finished_run,
        }) if task_id == task.task_id && finished_run == run_id => break,
        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
        Err(broadcast::error::RecvError::Closed) => break,
      }
    }
    write_frame(stream, &ServerMessage::LogsFinished)
      .await
      .map_err(RequestError::internal)
  }

  async fn append_log(&self, task_id: String, run_id: String, stream: LogStream, data: Vec<u8>) {
    let mut logs = self.logs.lock().await;
    let events = logs.entry(run_id.clone()).or_default();
    let sequence = events.last().map_or(0, |event| event.sequence + 1);
    let event = LogEvent {
      run_id,
      sequence,
      stream,
      data,
    };
    events.push(event.clone());
    trim_logs(events);
    drop(logs);
    let _ = self.activity.send(Activity::Log { task_id, event });
  }
}

/// Runs taskd until it receives an interrupt or encounters an endpoint error.
///
/// # Errors
///
/// Returns an error when local storage, the Unix endpoint, or signal handling
/// cannot be initialized or operated safely.
pub async fn run(config: DaemonConfig) -> Result<(), DaemonError> {
  #[cfg(unix)]
  prepare_runtime_directory(&config.socket_path)?;
  #[cfg(unix)]
  let listener = bind_listener(&config.socket_path).await?;
  #[cfg(windows)]
  let listener = task_ipc::windows::bind(&config.socket_path).map_err(DaemonError::Socket)?;
  #[cfg(unix)]
  let _socket_guard = SocketGuard(config.socket_path.clone());
  let state = Arc::new(State::load(&config.data_directory, config.rmux_socket)?);
  let reconciliation = state.reconcile_interactive();
  tokio::pin!(reconciliation);
  loop {
    tokio::select! {
      () = &mut reconciliation => unreachable!("reconciliation runs until shutdown"),
      accepted = listener.accept() => {
        #[cfg(unix)]
        let (stream, _) = accepted.map_err(DaemonError::Accept)?;
        #[cfg(windows)]
        let stream = accepted.map_err(DaemonError::Accept)?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
          let _ = handle_connection(stream, state).await;
        });
      }
      signal = tokio::signal::ctrl_c() => {
        signal.map_err(DaemonError::Signal)?;
        return Ok(());
      }
    }
  }
}

async fn handle_connection(
  mut stream: Stream,
  state: Arc<State>,
) -> Result<(), task_proto::CodecError> {
  let handshake = read_frame::<_, ClientMessage>(&mut stream).await?;
  match handshake {
    Some(ClientMessage::Handshake {
      protocol_version, ..
    }) if protocol_version == PROTOCOL_VERSION => {
      write_frame(
        &mut stream,
        &ServerMessage::HandshakeAccepted {
          protocol_version: PROTOCOL_VERSION,
        },
      )
      .await?;
    }
    Some(ClientMessage::Handshake { .. }) => {
      send_error(
        &mut stream,
        RequestError::new(
          ErrorCode::ProtocolVersionMismatch,
          format!("task protocol version {PROTOCOL_VERSION} is required"),
        ),
      )
      .await?;
      return Ok(());
    }
    _ => {
      send_error(
        &mut stream,
        RequestError::new(
          ErrorCode::InvalidRequest,
          "handshake must be the first message",
        ),
      )
      .await?;
      return Ok(());
    }
  }

  let Some(request) = read_frame::<_, ClientMessage>(&mut stream).await? else {
    return Ok(());
  };
  handle_request(&mut stream, &state, request).await
}

async fn handle_request(
  stream: &mut Stream,
  state: &Arc<State>,
  request: ClientMessage,
) -> Result<(), task_proto::CodecError> {
  // Serialize state-changing requests through publication of each runtime handle.
  // Log followers must not hold this guard, since stop may be their next event.
  let _mutation = if matches!(
    &request,
    ClientMessage::CreateTask { .. }
      | ClientMessage::StartTask { .. }
      | ClientMessage::StopTask { .. }
      | ClientMessage::RestartTask { .. }
      | ClientMessage::RemoveTask { .. }
  ) {
    Some(state.mutations.lock().await)
  } else {
    None
  };
  let result = match request {
    ClientMessage::CreateTask { definition } => state
      .create(definition)
      .await
      .map(|task| ServerMessage::TaskCreated { task }),
    ClientMessage::ListTasks => Ok(ServerMessage::TaskList {
      tasks: state.list().await,
    }),
    ClientMessage::ShowTask { task } => state
      .show(&task)
      .await
      .map(|task| ServerMessage::TaskStatus { task }),
    ClientMessage::StartTask { task } => state
      .start(&task)
      .await
      .map(|task| ServerMessage::TaskStatus { task }),
    ClientMessage::StopTask { task } => state
      .stop(&task)
      .await
      .map(|task| ServerMessage::TaskStatus { task }),
    ClientMessage::RestartTask { task } => state
      .restart(&task)
      .await
      .map(|task| ServerMessage::TaskStatus { task }),
    ClientMessage::RemoveTask { task } => state
      .remove(&task)
      .await
      .map(|task_id| ServerMessage::TaskRemoved { task_id }),
    ClientMessage::ReadLogs {
      task,
      after_sequence,
      follow,
    } => {
      if let Err(error) = state.send_logs(stream, &task, after_sequence, follow).await {
        send_error(stream, error).await?;
      }
      return Ok(());
    }
    ClientMessage::Handshake { .. } => Err(RequestError::new(
      ErrorCode::InvalidRequest,
      "handshake was already completed",
    )),
  };
  match result {
    Ok(response) => write_frame(stream, &response).await?,
    Err(error) => send_error(stream, error).await?,
  }
  Ok(())
}

fn spawn_log_reader<R>(
  state: Arc<State>,
  task_id: String,
  run_id: String,
  stream: LogStream,
  mut reader: R,
) -> tokio::task::JoinHandle<()>
where
  R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
  tokio::spawn(async move {
    let mut buffer = vec![0; 8192];
    loop {
      match reader.read(&mut buffer).await {
        Ok(0) | Err(_) => return,
        Ok(length) => {
          state
            .append_log(
              task_id.clone(),
              run_id.clone(),
              stream,
              buffer[..length].to_vec(),
            )
            .await;
        }
      }
    }
  })
}

fn trim_logs(events: &mut Vec<LogEvent>) {
  let mut bytes: usize = events.iter().map(|event| event.data.len()).sum();
  let remove_count = events
    .iter()
    .take_while(|event| {
      if bytes <= MAX_LOG_BYTES_PER_RUN {
        return false;
      }
      bytes = bytes.saturating_sub(event.data.len());
      true
    })
    .count();
  events.drain(..remove_count);
}

async fn send_error(
  stream: &mut Stream,
  error: RequestError,
) -> Result<(), task_proto::CodecError> {
  write_frame(
    stream,
    &ServerMessage::Error {
      code: error.code,
      message: error.message,
    },
  )
  .await
}

fn resolve_task<'a>(
  tasks: &'a BTreeMap<String, TaskInfo>,
  selector: &str,
) -> Result<&'a TaskInfo, RequestError> {
  tasks
    .get(selector)
    .or_else(|| tasks.values().find(|task| task.definition.name == selector))
    .ok_or_else(|| {
      RequestError::new(
        ErrorCode::TaskNotFound,
        format!("task {selector:?} was not found"),
      )
    })
}

fn resolve_task_mut<'a>(
  tasks: &'a mut BTreeMap<String, TaskInfo>,
  selector: &str,
) -> Result<&'a mut TaskInfo, RequestError> {
  let task_id = resolve_task(tasks, selector)?.task_id.clone();
  Ok(tasks.get_mut(&task_id).expect("resolved task exists"))
}

fn validate_definition(definition: &TaskDefinition) -> Result<(), RequestError> {
  if definition.name.is_empty() || definition.name.len() > 64 {
    return Err(RequestError::new(
      ErrorCode::InvalidDefinition,
      "task names must contain between 1 and 64 bytes",
    ));
  }
  if !definition
    .name
    .bytes()
    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
  {
    return Err(RequestError::new(
      ErrorCode::InvalidDefinition,
      "task names may contain only ASCII letters, digits, '-', '_', and '.'",
    ));
  }
  if definition.program.is_empty() {
    return Err(RequestError::new(
      ErrorCode::InvalidDefinition,
      "task program must not be empty",
    ));
  }
  Ok(())
}

fn now_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis()
    .try_into()
    .unwrap_or(u64::MAX)
}

#[must_use]
pub fn default_data_directory() -> PathBuf {
  env::var_os("TASKD_DATA_DIR").map_or_else(
    || {
      dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ctl/taskd")
    },
    PathBuf::from,
  )
}

#[cfg(unix)]
fn prepare_runtime_directory(socket_path: &Path) -> Result<(), DaemonError> {
  let directory = socket_path.parent().ok_or_else(|| {
    DaemonError::RuntimeDirectory(io::Error::new(
      io::ErrorKind::InvalidInput,
      "taskd socket has no parent directory",
    ))
  })?;
  fs::create_dir_all(directory).map_err(DaemonError::RuntimeDirectory)?;
  fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
    .map_err(DaemonError::RuntimeDirectory)
}

fn prepare_data_directory(directory: &Path) -> Result<(), DaemonError> {
  fs::create_dir_all(directory).map_err(DaemonError::DataDirectory)?;
  #[cfg(unix)]
  fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
    .map_err(DaemonError::DataDirectory)?;
  Ok(())
}

#[cfg(unix)]
async fn bind_listener(path: &Path) -> Result<UnixListener, DaemonError> {
  let listener = match UnixListener::bind(path) {
    Ok(listener) => listener,
    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
      if Stream::connect(path).await.is_ok() {
        return Err(DaemonError::AlreadyRunning(path.into()));
      }
      let metadata = fs::symlink_metadata(path).map_err(DaemonError::Socket)?;
      if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != rustix::process::getuid().as_raw()
      {
        return Err(DaemonError::UnsafeSocket(path.into()));
      }
      fs::remove_file(path).map_err(DaemonError::Socket)?;
      UnixListener::bind(path).map_err(DaemonError::Socket)?
    }
    Err(error) => return Err(DaemonError::Socket(error)),
  };
  fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(DaemonError::Socket)?;
  Ok(listener)
}

#[cfg(unix)]
struct SocketGuard(PathBuf);

#[cfg(unix)]
impl Drop for SocketGuard {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
  }
}

#[derive(Debug)]
struct RequestError {
  code: ErrorCode,
  message: String,
}

impl RequestError {
  fn new(code: ErrorCode, message: impl Into<String>) -> Self {
    Self {
      code,
      message: message.into(),
    }
  }

  fn internal(error: impl std::fmt::Display) -> Self {
    Self::new(ErrorCode::Internal, error.to_string())
  }
}

#[derive(Debug, Error)]
pub enum DaemonError {
  #[error("could not prepare taskd runtime directory: {0}")]
  RuntimeDirectory(#[source] io::Error),
  #[error("could not prepare taskd data directory: {0}")]
  DataDirectory(#[source] io::Error),
  #[error("could not bind taskd socket: {0}")]
  Socket(#[source] io::Error),
  #[error("taskd is already running at {}", .0.display())]
  AlreadyRunning(PathBuf),
  #[error("refusing to replace unsafe socket path {}", .0.display())]
  UnsafeSocket(PathBuf),
  #[error("could not accept taskd connection: {0}")]
  Accept(#[source] io::Error),
  #[error("could not read task state: {0}")]
  ReadState(#[source] io::Error),
  #[error("invalid task state: {0}")]
  ParseState(#[source] serde_json::Error),
  #[error("unsupported task state schema version {0}")]
  UnsupportedStateVersion(u16),
  #[error("could not serialize task state: {0}")]
  SerializeState(#[source] serde_json::Error),
  #[error("could not write task state: {0}")]
  WriteState(#[source] io::Error),
  #[error("could not listen for shutdown: {0}")]
  Signal(#[source] io::Error),
}
