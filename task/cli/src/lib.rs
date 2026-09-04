use clap::{Subcommand, ValueEnum};
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use task_ipc::{Stream, connect, socket_path};
use task_proto::{
  ClientMessage, ExecutionMode, PROTOCOL_VERSION, ServerMessage, TaskDefinition, TaskInfo,
  read_frame, write_frame,
};
use thiserror::Error;
use tokio::time::sleep;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Subcommand)]
pub enum Command {
  Create {
    name: String,
    #[arg(long)]
    cwd: Option<String>,
    #[arg(long, value_enum, default_value_t = Mode::Background)]
    mode: Mode,
    #[arg(long)]
    start: bool,
    #[arg(last = true, required = true)]
    command: Vec<String>,
  },
  List,
  Show {
    task: String,
  },
  Start {
    task: String,
  },
  Stop {
    task: String,
  },
  Restart {
    task: String,
  },
  Logs {
    task: String,
    #[arg(long)]
    follow: bool,
    #[arg(long)]
    after: Option<u64>,
  },
  Remove {
    task: String,
  },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Mode {
  Interactive,
  Background,
}

impl From<Mode> for ExecutionMode {
  fn from(value: Mode) -> Self {
    match value {
      Mode::Interactive => Self::Interactive,
      Mode::Background => Self::Background,
    }
  }
}

/// Runs one task command against the local per-user taskd.
///
/// # Errors
///
/// Returns an error when taskd cannot be reached, a protocol exchange fails,
/// taskd rejects the operation, or log output cannot be written.
pub async fn run(mut command: Command) -> Result<(), CommandError> {
  if let Command::Create { cwd, .. } = &mut command {
    let current = env::current_dir()?;
    let directory = cwd
      .as_ref()
      .map_or_else(|| current.clone(), |path| current.join(path));
    *cwd = Some(
      directory
        .to_str()
        .ok_or_else(|| {
          io::Error::new(
            io::ErrorKind::InvalidInput,
            "task working directory must be valid Unicode",
          )
        })?
        .to_owned(),
    );
  }
  let socket = socket_path();
  let mut stream = connect_or_start(&socket).await?;
  request(
    &mut stream,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: "ctl".into(),
    },
  )
  .await?;
  expect_handshake(read_required(&mut stream).await?)?;

  execute(command, stream, &socket).await
}

async fn execute(command: Command, mut stream: Stream, socket: &Path) -> Result<(), CommandError> {
  match command {
    Command::Create {
      name,
      cwd,
      mode,
      start,
      mut command,
    } => {
      let program = command.remove(0);
      let response = one(
        &mut stream,
        ClientMessage::CreateTask {
          definition: TaskDefinition {
            name,
            program,
            arguments: command,
            working_directory: cwd,
            execution_mode: mode.into(),
          },
        },
      )
      .await?;
      let task = expect_task(response)?;
      print_task(&task);
      if start {
        let mut stream = connect_or_start(socket).await?;
        handshake(&mut stream).await?;
        print_task(&expect_task(
          one(&mut stream, ClientMessage::StartTask { task: task.task_id }).await?,
        )?);
      }
    }
    Command::List => match one(&mut stream, ClientMessage::ListTasks).await? {
      ServerMessage::TaskList { tasks } => {
        for task in tasks {
          print_task(&task);
        }
      }
      response => return Err(unexpected("task_list", &response)),
    },
    Command::Show { task } => {
      print_task(&expect_task(
        one(&mut stream, ClientMessage::ShowTask { task }).await?,
      )?);
    }
    Command::Start { task } => {
      print_task(&expect_task(
        one(&mut stream, ClientMessage::StartTask { task }).await?,
      )?);
    }
    Command::Stop { task } => {
      print_task(&expect_task(
        one(&mut stream, ClientMessage::StopTask { task }).await?,
      )?);
    }
    Command::Restart { task } => {
      print_task(&expect_task(
        one(&mut stream, ClientMessage::RestartTask { task }).await?,
      )?);
    }
    Command::Remove { task } => match one(&mut stream, ClientMessage::RemoveTask { task }).await? {
      ServerMessage::TaskRemoved { task_id } => println!("removed {task_id}"),
      response => return Err(unexpected("task_removed", &response)),
    },
    Command::Logs {
      task,
      follow,
      after,
    } => print_logs(&mut stream, task, follow, after).await?,
  }
  Ok(())
}

async fn print_logs(
  stream: &mut Stream,
  task: String,
  follow: bool,
  after_sequence: Option<u64>,
) -> Result<(), CommandError> {
  request(
    stream,
    &ClientMessage::ReadLogs {
      task,
      after_sequence,
      follow,
    },
  )
  .await?;
  loop {
    match read_required(stream).await? {
      ServerMessage::Log { event } => {
        let output: &mut dyn Write = match event.stream {
          task_proto::LogStream::Stdout => &mut io::stdout(),
          task_proto::LogStream::Stderr => &mut io::stderr(),
        };
        output.write_all(&event.data)?;
        output.flush()?;
      }
      ServerMessage::LogsFinished => return Ok(()),
      ServerMessage::Error { code, message } => {
        return Err(CommandError::Server { code, message });
      }
      response => return Err(unexpected("log or logs_finished", &response)),
    }
  }
}

fn print_task(task: &TaskInfo) {
  let state = task.active_run.as_ref().map_or_else(
    || {
      task
        .last_run
        .as_ref()
        .map_or("stopped", |run| match run.state {
          task_proto::RunState::Running => "running",
          task_proto::RunState::Completed => "completed",
          task_proto::RunState::Failed => "failed",
          task_proto::RunState::Stopped => "stopped",
        })
    },
    |_| "running",
  );
  println!(
    "{}\t{}\t{}\t{}",
    task.task_id, task.definition.name, state, task.definition.program
  );
}

async fn handshake(stream: &mut Stream) -> Result<(), CommandError> {
  request(
    stream,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: "ctl".into(),
    },
  )
  .await?;
  expect_handshake(read_required(stream).await?)
}

async fn one(stream: &mut Stream, message: ClientMessage) -> Result<ServerMessage, CommandError> {
  request(stream, &message).await?;
  let response = read_required(stream).await?;
  if let ServerMessage::Error { code, message } = response {
    return Err(CommandError::Server { code, message });
  }
  Ok(response)
}

async fn request(stream: &mut Stream, message: &ClientMessage) -> Result<(), CommandError> {
  write_frame(stream, message).await?;
  Ok(())
}

async fn read_required(stream: &mut Stream) -> Result<ServerMessage, CommandError> {
  read_frame(stream)
    .await?
    .ok_or(CommandError::UnexpectedEndOfStream)
}

fn expect_handshake(response: ServerMessage) -> Result<(), CommandError> {
  match response {
    ServerMessage::HandshakeAccepted { protocol_version }
      if protocol_version == PROTOCOL_VERSION =>
    {
      Ok(())
    }
    ServerMessage::Error { code, message } => Err(CommandError::Server { code, message }),
    response => Err(unexpected("handshake_accepted", &response)),
  }
}

fn expect_task(response: ServerMessage) -> Result<TaskInfo, CommandError> {
  match response {
    ServerMessage::TaskCreated { task } | ServerMessage::TaskStatus { task } => Ok(task),
    response => Err(unexpected("task status", &response)),
  }
}

fn unexpected(expected: &'static str, response: &ServerMessage) -> CommandError {
  CommandError::UnexpectedResponse {
    expected,
    actual: format!("{response:?}"),
  }
}

async fn connect_or_start(socket: &Path) -> Result<Stream, CommandError> {
  match connect(socket).await {
    Ok(stream) => return Ok(stream),
    Err(error) if retryable(&error) => {}
    Err(error) => return Err(CommandError::Connect(error)),
  }
  let executable = daemon_executable()?;
  let mut daemon = std::process::Command::new(&executable);
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS: taskd survives the invoking console.
    daemon.creation_flags(0x0000_0008);
  }
  daemon
    .arg("--socket")
    .arg(socket)
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| CommandError::StartDaemon { executable, source })?;
  let deadline = Instant::now() + CONNECT_TIMEOUT;
  loop {
    match connect(socket).await {
      Ok(stream) => return Ok(stream),
      Err(error) if retryable(&error) && Instant::now() < deadline => {
        sleep(Duration::from_millis(25)).await;
      }
      Err(error) => return Err(CommandError::Connect(error)),
    }
  }
}

fn daemon_executable() -> Result<PathBuf, CommandError> {
  if let Some(executable) = env::var_os("TASKD_BIN") {
    return Ok(PathBuf::from(executable));
  }
  let current = env::current_exe().map_err(CommandError::CurrentExecutable)?;
  let sibling = current.with_file_name(format!("taskd{}", env::consts::EXE_SUFFIX));
  if sibling.is_file() {
    return Ok(sibling);
  }
  Ok(PathBuf::from(format!("taskd{}", env::consts::EXE_SUFFIX)))
}

fn retryable(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
  )
}

#[derive(Debug, Error)]
pub enum CommandError {
  #[error(transparent)]
  Codec(#[from] task_proto::CodecError),
  #[error("could not connect to taskd: {0}")]
  Connect(#[source] io::Error),
  #[error("could not determine the current executable: {0}")]
  CurrentExecutable(#[source] io::Error),
  #[error("could not start taskd using {}: {source}", executable.display())]
  StartDaemon {
    executable: PathBuf,
    source: io::Error,
  },
  #[error("taskd closed the connection before responding")]
  UnexpectedEndOfStream,
  #[error("expected {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: String,
  },
  #[error("taskd error {code:?}: {message}")]
  Server {
    code: task_proto::ErrorCode,
    message: String,
  },
  #[error("terminal output failed: {0}")]
  Output(#[from] io::Error),
}
