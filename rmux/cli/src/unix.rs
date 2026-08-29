use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use rmux_proto::{
  ClientMessage, CodecError, CommandSpec, PROTOCOL_VERSION, ServerMessage, SessionInfo,
  TerminalSize, read_frame, write_frame,
};
use std::env;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::{Instant, sleep};

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const DETACH_BYTE: u8 = 0x1d;

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
  let response = request(
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
  let response = request(socket_path, ClientMessage::ListSessions).await?;
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
  let response = request(
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
) -> Result<(), ClientError> {
  let mut stream = connect_and_handshake(socket_path).await?;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: session.into(),
      resume_from,
    },
  )
  .await?;

  let response = read_response(&mut stream).await?;
  let ServerMessage::Attached {
    session,
    replay_from,
    history_gap,
    ..
  } = response
  else {
    return Err(unexpected("attached", &response));
  };

  if history_gap {
    eprintln!(
      "rmux: requested history is no longer retained; replaying from sequence {replay_from}"
    );
  }
  eprintln!("[attached to {}; press Ctrl-] to detach]", session.name);

  let interactive = io::stdin().is_terminal();
  let _raw_mode = RawModeGuard::enable_if(interactive)?;
  let (mut socket_reader, mut socket_writer) = stream.into_split();

  let input = async {
    let mut stdin = tokio::io::stdin();
    let mut buffer = vec![0_u8; 4096];
    loop {
      let bytes_read = stdin.read(&mut buffer).await?;
      if bytes_read == 0 {
        write_frame(&mut socket_writer, &ClientMessage::Detach).await?;
        return Ok::<(), ClientError>(());
      }

      let input = &buffer[..bytes_read];
      if let Some(detach_at) = input.iter().position(|byte| *byte == DETACH_BYTE) {
        if detach_at > 0 {
          write_frame(
            &mut socket_writer,
            &ClientMessage::Input {
              data: input[..detach_at].to_vec(),
            },
          )
          .await?;
        }
        write_frame(&mut socket_writer, &ClientMessage::Detach).await?;
        return Ok(());
      }

      write_frame(
        &mut socket_writer,
        &ClientMessage::Input {
          data: input.to_vec(),
        },
      )
      .await?;
    }
  };

  let output = async {
    let mut stdout = tokio::io::stdout();
    loop {
      let Some(message) = read_frame::<_, ServerMessage>(&mut socket_reader).await? else {
        return Ok::<(), ClientError>(());
      };
      match message {
        ServerMessage::Output { data, .. } => {
          stdout.write_all(&data).await?;
          stdout.flush().await?;
        }
        ServerMessage::SessionEnded { exit_code, .. } => {
          stdout.flush().await?;
          eprintln!("\r\n[session ended with exit code {exit_code:?}]");
          return Ok(());
        }
        ServerMessage::Error { code, message } => {
          return Err(ClientError::Server { code, message });
        }
        response => return Err(unexpected("output or session_ended", &response)),
      }
    }
  };

  tokio::select! {
    result = input => result,
    result = output => result,
  }
}

async fn request(socket_path: &Path, message: ClientMessage) -> Result<ServerMessage, ClientError> {
  let mut stream = connect_and_handshake(socket_path).await?;
  write_frame(&mut stream, &message).await?;
  read_response(&mut stream).await
}

async fn connect_and_handshake(socket_path: &Path) -> Result<UnixStream, ClientError> {
  let mut stream = connect_or_start_daemon(socket_path).await?;
  write_frame(
    &mut stream,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: "rmux".into(),
      client_version: CLIENT_VERSION.into(),
    },
  )
  .await?;

  match read_response(&mut stream).await? {
    ServerMessage::HandshakeAccepted {
      protocol_version, ..
    } if protocol_version == PROTOCOL_VERSION => Ok(stream),
    response => Err(unexpected("handshake_accepted", &response)),
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

async fn read_response(stream: &mut UnixStream) -> Result<ServerMessage, ClientError> {
  match read_frame(stream).await? {
    Some(ServerMessage::Error { code, message }) => Err(ClientError::Server { code, message }),
    Some(message) => Ok(message),
    None => Err(ClientError::UnexpectedEof),
  }
}

fn current_terminal_size() -> TerminalSize {
  let (columns, rows) = size().unwrap_or((80, 24));
  TerminalSize {
    columns,
    rows,
    pixel_width: 0,
    pixel_height: 0,
  }
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

struct RawModeGuard {
  enabled: bool,
}

impl RawModeGuard {
  fn enable_if(enabled: bool) -> Result<Self, ClientError> {
    if enabled {
      enable_raw_mode()?;
    }
    Ok(Self { enabled })
  }
}

impl Drop for RawModeGuard {
  fn drop(&mut self) {
    if self.enabled {
      let _ignored = disable_raw_mode();
    }
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
  Codec(#[from] CodecError),
  #[error("terminal I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("daemon closed the connection before responding")]
  UnexpectedEof,
  #[error("daemon error {code:?}: {message}")]
  Server {
    code: rmux_proto::ErrorCode,
    message: String,
  },
  #[error("expected {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: String,
  },
}
