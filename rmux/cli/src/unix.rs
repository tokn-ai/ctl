use rmux_client::{
  AttachExitReason, AttachRequest, ClientIdentity, attach_interactive, begin_attach,
  current_terminal_size, request,
};
use rmux_proto::{ClientMessage, CommandSpec, ServerMessage, SessionInfo};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::net::UnixStream;
use tokio::time::{Instant, sleep};

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn create_session(
  socket_path: &Path,
  name: Option<String>,
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> Result<(), ClientError> {
  let terminal_size = current_terminal_size();
  let working_directory = match working_directory {
    Some(directory) => Some(directory),
    None => Some(current_working_directory()?),
  };
  let response = local_request(
    socket_path,
    ClientMessage::CreateSession {
      name,
      command,
      working_directory,
      terminal_size,
    },
  )
  .await?;

  match response {
    ServerMessage::SessionCreated { session } => {
      println!("{}\t{}", session.session_id, session.name);
      Ok(())
    }
    response => Err(unexpected("session_created", &response)),
  }
}

pub async fn list_sessions(socket_path: &Path) -> Result<(), ClientError> {
  let response = local_request(socket_path, ClientMessage::ListSessions).await?;
  match response {
    ServerMessage::SessionList { sessions } => {
      if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
      }

      println!("NAME\tID\tSTATUS\tSIZE\tNEXT_SEQUENCE");
      for session in sessions {
        print_session(&session);
      }
      Ok(())
    }
    response => Err(unexpected("session_list", &response)),
  }
}

pub async fn kill_session(socket_path: &Path, session: &str) -> Result<(), ClientError> {
  let response = local_request(
    socket_path,
    ClientMessage::KillSession {
      session: session.into(),
    },
  )
  .await?;
  match response {
    ServerMessage::Success => Ok(()),
    response => Err(unexpected("success", &response)),
  }
}

pub async fn attach_session(
  socket_path: &Path,
  session: &str,
  resume_from: Option<u64>,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> Result<(), ClientError> {
  let stream = connect_or_start_daemon(socket_path).await?;
  let identity = client_identity();
  let (stream, attached) = begin_attach(
    stream,
    &identity,
    AttachRequest {
      session: session.into(),
      resume_from,
      terminal_size: current_terminal_size(),
      request_input_lease,
      request_layout_lease,
    },
  )
  .await?;
  let exit = attach_interactive(stream, &attached).await?;
  if matches!(exit.reason, AttachExitReason::ConnectionClosed) {
    eprintln!("rmux: attachment connection closed; the session may still be running");
  }
  Ok(())
}

async fn local_request(
  socket_path: &Path,
  message: ClientMessage,
) -> Result<ServerMessage, ClientError> {
  let stream = connect_or_start_daemon(socket_path).await?;
  let identity = client_identity();
  Ok(request(stream, &identity, message).await?)
}

fn client_identity() -> ClientIdentity {
  ClientIdentity {
    name: "rmux".into(),
    version: CLIENT_VERSION.into(),
  }
}

async fn connect_or_start_daemon(socket_path: &Path) -> Result<UnixStream, ClientError> {
  match UnixStream::connect(socket_path).await {
    Ok(stream) => return Ok(stream),
    Err(error)
      if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
      ) => {}
    Err(error) => return Err(ClientError::Connect(error)),
  }

  start_daemon(socket_path)?;
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    match UnixStream::connect(socket_path).await {
      Ok(stream) => return Ok(stream),
      Err(error)
        if Instant::now() < deadline
          && matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
          ) =>
      {
        sleep(Duration::from_millis(25)).await;
      }
      Err(error) => return Err(ClientError::Connect(error)),
    }
  }
}

fn start_daemon(socket_path: &Path) -> Result<(), ClientError> {
  let executable = daemon_executable()?;
  std::process::Command::new(&executable)
    .arg("--socket")
    .arg(socket_path)
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| ClientError::StartDaemon { executable, source })?;
  Ok(())
}

fn daemon_executable() -> Result<PathBuf, ClientError> {
  if let Some(executable) = env::var_os("RMUXD_BIN") {
    return Ok(PathBuf::from(executable));
  }

  let current_executable = env::current_exe().map_err(ClientError::CurrentExecutable)?;
  let sibling = current_executable.with_file_name("rmuxd");
  if sibling.is_file() {
    return Ok(sibling);
  }

  Ok(PathBuf::from("rmuxd"))
}

fn current_working_directory() -> Result<String, ClientError> {
  let directory = env::current_dir().map_err(ClientError::CurrentDirectory)?;
  directory
    .into_os_string()
    .into_string()
    .map_err(ClientError::NonUtf8CurrentDirectory)
}

fn print_session(session: &SessionInfo) {
  println!(
    "{}\t{}\t{:?}\t{}x{}\t{}",
    session.name,
    session.session_id,
    session.status,
    session.terminal_size.columns,
    session.terminal_size.rows,
    session.next_sequence
  );
}

fn unexpected(expected: &'static str, response: &ServerMessage) -> ClientError {
  ClientError::UnexpectedResponse {
    expected,
    actual: format!("{response:?}"),
  }
}

#[derive(Debug, Error)]
pub enum ClientError {
  #[error("could not connect to rmuxd: {0}")]
  Connect(io::Error),
  #[error("could not determine the current executable: {0}")]
  CurrentExecutable(io::Error),
  #[error("could not determine the current working directory: {0}")]
  CurrentDirectory(io::Error),
  #[error("the current working directory is not valid UTF-8: {0:?}")]
  NonUtf8CurrentDirectory(std::ffi::OsString),
  #[error("could not start daemon using {}: {source}", executable.display())]
  StartDaemon {
    executable: PathBuf,
    source: io::Error,
  },
  #[error(transparent)]
  Protocol(#[from] rmux_client::ClientError),
  #[error("expected {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: String,
  },
}
