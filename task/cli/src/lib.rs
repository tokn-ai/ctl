use clap::{Subcommand, ValueEnum};

use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;
use task_client::connect_or_start;

use task_ipc::{Stream, socket_path};
use task_proto::{
  ClientMessage, ExecutionMode, PROTOCOL_VERSION, ServerMessage, TaskDefinition, TaskInfo,
  read_frame, write_frame,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;

#[cfg(test)]
mod tests;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub type ConnectFuture<'a, Stream, ConnectError> =
  Pin<Box<dyn Future<Output = Result<Stream, ConnectError>> + Send + 'a>>;
pub type AttachFuture<'a> = Pin<Box<dyn Future<Output = Result<(), CommandError>> + Send + 'a>>;

/// Routes task commands and their interactive sessions to the same target.
pub trait Connector {
  type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;
  type Error: std::error::Error + Send + Sync + 'static;

  fn connect_task(&self) -> ConnectFuture<'_, Self::Stream, Self::Error>;
  fn is_local_task_target(&self) -> bool;

  /// Attaches to the selected target's rmux session. The socket path belongs to
  /// that target and must only be used as local IPC for a local target.
  fn attach_interactive(&self, session: String, rmux_socket: PathBuf) -> AttachFuture<'_>;
}

/// Connector for the local per-user task daemon and its rmux sessions.
#[derive(Debug, Clone)]
pub struct LocalConnector {
  socket_path: PathBuf,
}

impl LocalConnector {
  #[must_use]
  pub fn new(socket_path: PathBuf) -> Self {
    Self { socket_path }
  }
}

impl Connector for LocalConnector {
  type Stream = Stream;
  type Error = task_client::ClientError;

  fn connect_task(&self) -> ConnectFuture<'_, Self::Stream, Self::Error> {
    Box::pin(connect_or_start(&self.socket_path))
  }

  fn is_local_task_target(&self) -> bool {
    true
  }

  fn attach_interactive(&self, session: String, rmux_socket: PathBuf) -> AttachFuture<'_> {
    Box::pin(
      async move { attach_session(session, &rmux_cli::LocalConnector::new(rmux_socket)).await },
    )
  }
}

/// Runs the interactive rmux attachment through the target's connector.
///
/// # Errors
///
/// Returns an error when the session cannot be reached or terminal attachment fails.
pub async fn attach_session<C: rmux_cli::Connector>(
  session: String,
  connector: &C,
) -> Result<(), CommandError> {
  rmux_cli::run(
    rmux_cli::Command::Attach {
      session,
      resume_from: None,
      read_only: false,
      resize: true,
    },
    connector,
  )
  .await?;
  Ok(())
}

#[derive(Debug, Subcommand)]
pub enum Command {
  Create {
    name: String,
    /// Working directory on the target; defaults to the local cwd or remote home.
    #[arg(long)]
    cwd: Option<String>,
    #[arg(long, value_enum, default_value_t = Mode::Background)]
    mode: Mode,
    #[arg(long)]
    start: bool,
    #[arg(last = true, required = true)]
    command: Vec<String>,
  },
  Attach {
    task: String,
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
pub async fn run(command: Command) -> Result<(), CommandError> {
  run_with_connector(command, &LocalConnector::new(socket_path())).await
}

/// Runs one task command against the supplied local or remote target.
///
/// # Errors
///
/// Returns connection, protocol, daemon, working directory, or output errors.
pub async fn run_with_connector<C: Connector>(
  mut command: Command,
  connector: &C,
) -> Result<(), CommandError> {
  if connector.is_local_task_target()
    && let Command::Create { cwd, .. } = &mut command
  {
    let current = std::env::current_dir().map_err(CommandError::WorkingDirectory)?;
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
        })
        .map_err(CommandError::WorkingDirectory)?
        .to_owned(),
    );
  }
  let stream = connect(connector).await?;
  execute(command, stream, connector).await
}

async fn connect<C: Connector>(connector: &C) -> Result<C::Stream, CommandError> {
  let mut stream = connector
    .connect_task()
    .await
    .map_err(|error| CommandError::Connect(Box::new(error)))?;
  timeout(HANDSHAKE_TIMEOUT, handshake(&mut stream))
    .await
    .map_err(|_| CommandError::Timeout {
      operation: "handshake",
    })??;
  Ok(stream)
}

async fn attach_task<C: Connector>(task: &TaskInfo, connector: &C) -> Result<(), CommandError> {
  let backend = task
    .active_run
    .as_ref()
    .and_then(|run| run.interactive.as_ref())
    .ok_or_else(|| CommandError::Server {
      code: task_proto::ErrorCode::NotRunning,
      message: "task has no active interactive run".into(),
    })?;
  let session = backend
    .session_id
    .clone()
    .ok_or_else(|| CommandError::Server {
      code: task_proto::ErrorCode::NotRunning,
      message: "interactive session is not ready".into(),
    })?;
  connector
    .attach_interactive(session, backend.rmux_socket.clone())
    .await
}

async fn execute<C: Connector>(
  command: Command,
  mut stream: C::Stream,
  connector: &C,
) -> Result<(), CommandError> {
  match command {
    Command::Create {
      name,
      cwd,
      mode,
      start,
      command,
    } => {
      let mut command = command.into_iter();
      let program = command.next().ok_or(CommandError::MissingProgram)?;
      let response = one(
        &mut stream,
        ClientMessage::CreateTask {
          definition: TaskDefinition {
            name,
            program,
            arguments: command.collect(),
            working_directory: cwd,
            execution_mode: mode.into(),
          },
        },
      )
      .await?;
      let task = expect_task(response)?;
      drop(stream);
      print_task(&task);
      if start {
        let mut stream = connect(connector).await?;
        print_task(&expect_task(
          one(&mut stream, ClientMessage::StartTask { task: task.task_id }).await?,
        )?);
      }
    }
    Command::Attach { task } => {
      let task = expect_task(one(&mut stream, ClientMessage::ShowTask { task }).await?)?;
      drop(stream);
      attach_task(&task, connector).await?;
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

async fn print_logs<S: AsyncRead + AsyncWrite + Unpin>(
  stream: &mut S,
  task: String,
  follow: bool,
  after_sequence: Option<u64>,
) -> Result<(), CommandError> {
  timeout(
    REQUEST_TIMEOUT,
    request(
      stream,
      &ClientMessage::ReadLogs {
        task,
        after_sequence,
        follow,
      },
    ),
  )
  .await
  .map_err(|_| CommandError::Timeout {
    operation: "log request",
  })??;
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
  let state =
    task
      .active_run
      .as_ref()
      .or(task.last_run.as_ref())
      .map_or("stopped", |run| match run.state {
        task_proto::RunState::Starting => "starting",
        task_proto::RunState::Unknown => "unknown",
        task_proto::RunState::Running => "running",
        task_proto::RunState::Completed => "completed",
        task_proto::RunState::Failed => "failed",
        task_proto::RunState::Stopped => "stopped",
      });
  println!(
    "{}\t{}\t{}\t{}",
    task.task_id, task.definition.name, state, task.definition.program
  );
  if let Some(backend) = task
    .active_run
    .as_ref()
    .or(task.last_run.as_ref())
    .and_then(|run| run.interactive.as_ref())
    && let Some(session) = &backend.session_id
  {
    println!("  rmux session: {session}");
  }
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<(), CommandError> {
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

async fn one<S: AsyncRead + AsyncWrite + Unpin>(
  stream: &mut S,
  message: ClientMessage,
) -> Result<ServerMessage, CommandError> {
  timeout(REQUEST_TIMEOUT, async {
    request(stream, &message).await?;
    let response = read_required(stream).await?;
    if let ServerMessage::Error { code, message } = response {
      return Err(CommandError::Server { code, message });
    }
    Ok(response)
  })
  .await
  .map_err(|_| CommandError::Timeout {
    operation: "request",
  })?
}

async fn request<S: AsyncWrite + Unpin>(
  stream: &mut S,
  message: &ClientMessage,
) -> Result<(), CommandError> {
  write_frame(stream, message).await?;
  Ok(())
}

async fn read_required<S: AsyncRead + Unpin>(
  stream: &mut S,
) -> Result<ServerMessage, CommandError> {
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

#[derive(Debug, Error)]
pub enum CommandError {
  #[error(transparent)]
  Client(#[from] task_client::ClientError),
  #[error(transparent)]
  Rmux(#[from] rmux_cli::CommandError),
  #[error("could not resolve task working directory: {0}")]
  WorkingDirectory(#[source] io::Error),
  #[error("task command must include a program")]
  MissingProgram,
  #[error("taskd {operation} timed out")]
  Timeout { operation: &'static str },
  #[error(transparent)]
  Codec(#[from] task_proto::CodecError),
  #[error("could not connect to taskd: {0}")]
  Connect(#[source] Box<dyn std::error::Error + Send + Sync>),
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
