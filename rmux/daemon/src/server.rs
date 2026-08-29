use crate::session::{
  Session, SessionControlError, SessionEvent, SessionManager, SessionManagerError,
};
use rmux_core::{JournalError, OutputChunk};
use rmux_proto::{
  ClientMessage, CodecError, ErrorCode, PROTOCOL_VERSION, ServerMessage, read_frame, write_frame,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, broadcast};
use tokio::time::{Instant, sleep_until};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct DaemonConfig {
  pub socket_path: PathBuf,
  pub journal_capacity_bytes: usize,
  pub startup_idle_timeout: Duration,
}

impl Default for DaemonConfig {
  fn default() -> Self {
    Self {
      socket_path: rmux_ipc::socket_path(),
      journal_capacity_bytes: 4 * 1024 * 1024,
      startup_idle_timeout: Duration::from_secs(10),
    }
  }
}

#[derive(Debug, Error)]
pub enum DaemonError {
  #[error("could not prepare the runtime directory: {0}")]
  RuntimeDirectory(#[source] io::Error),
  #[error("another rmuxd is already listening at {0}")]
  AlreadyRunning(PathBuf),
  #[error("could not bind local socket {path}: {source}")]
  Bind { path: PathBuf, source: io::Error },
  #[error("daemon I/O error: {0}")]
  Io(#[from] io::Error),
}

/// Runs the daemon until its final session exits or an interrupt is received.
///
/// # Errors
///
/// Returns an error when the runtime directory or socket cannot be prepared,
/// the endpoint is already served, or the accept loop fails.
pub async fn run(config: DaemonConfig) -> Result<(), DaemonError> {
  rmux_ipc::prepare_runtime_directory(&config.socket_path)
    .map_err(DaemonError::RuntimeDirectory)?;
  let listener = bind_listener(&config.socket_path).await?;
  let _socket_guard = SocketGuard::new(config.socket_path.clone())?;
  let sessions = SessionManager::new(config.journal_capacity_bytes);
  let connections = Arc::new(ConnectionTracker::default());
  let startup_deadline = sleep_until(Instant::now() + config.startup_idle_timeout);
  tokio::pin!(startup_deadline);

  loop {
    if sessions.ever_had_session() && sessions.session_count() == 0 && connections.count() == 0 {
      return Ok(());
    }

    tokio::select! {
      accepted = listener.accept() => {
        let (stream, _) = accepted?;
        let sessions = sessions.clone();
        let connection_guard = connections.open();
        tokio::spawn(async move {
          if let Err(error) = handle_connection(stream, sessions).await {
            eprintln!("rmuxd connection error: {error}");
          }
          drop(connection_guard);
        });
      }
      () = sessions.changed() => {}
      () = connections.changed() => {}
      () = &mut startup_deadline, if !sessions.ever_had_session() => {
        if sessions.session_count() == 0 && connections.count() == 0 {
          return Ok(());
        }
        startup_deadline.as_mut().reset(Instant::now() + config.startup_idle_timeout);
      }
      result = tokio::signal::ctrl_c() => {
        result?;
        return Ok(());
      }
    }
  }
}

async fn bind_listener(path: &Path) -> Result<UnixListener, DaemonError> {
  match UnixListener::bind(path) {
    Ok(listener) => Ok(listener),
    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
      if UnixStream::connect(path).await.is_ok() {
        return Err(DaemonError::AlreadyRunning(path.to_path_buf()));
      }
      std::fs::remove_file(path).map_err(|source| DaemonError::Bind {
        path: path.to_path_buf(),
        source,
      })?;
      UnixListener::bind(path).map_err(|source| DaemonError::Bind {
        path: path.to_path_buf(),
        source,
      })
    }
    Err(source) => Err(DaemonError::Bind {
      path: path.to_path_buf(),
      source,
    }),
  }
}

async fn handle_connection(
  mut stream: UnixStream,
  sessions: SessionManager,
) -> Result<(), ConnectionError> {
  let Some(handshake) = read_frame::<_, ClientMessage>(&mut stream).await? else {
    return Ok(());
  };

  let ClientMessage::Handshake {
    protocol_version, ..
  } = handshake
  else {
    send_error(
      &mut stream,
      ErrorCode::InvalidRequest,
      "the first message must be a handshake",
    )
    .await?;
    return Ok(());
  };

  if protocol_version != PROTOCOL_VERSION {
    send_error(
      &mut stream,
      ErrorCode::ProtocolVersionMismatch,
      &format!(
        "client requested protocol version {protocol_version}; this daemon supports {PROTOCOL_VERSION}"
      ),
    )
    .await?;
    return Ok(());
  }

  write_frame(
    &mut stream,
    &ServerMessage::HandshakeAccepted {
      protocol_version: PROTOCOL_VERSION,
      server_version: SERVER_VERSION.into(),
    },
  )
  .await?;

  let Some(request) = read_frame::<_, ClientMessage>(&mut stream).await? else {
    return Ok(());
  };

  match request {
    ClientMessage::CreateSession {
      name,
      command,
      working_directory,
      terminal_size,
    } => match tokio::task::spawn_blocking(move || {
      sessions.create(name, command, working_directory, terminal_size)
    })
    .await?
    {
      Ok(session) => {
        write_frame(
          &mut stream,
          &ServerMessage::SessionCreated {
            session: session.info(),
          },
        )
        .await?;
      }
      Err(error) => send_session_manager_error(&mut stream, &error).await?,
    },
    ClientMessage::ListSessions => {
      write_frame(
        &mut stream,
        &ServerMessage::SessionList {
          sessions: sessions.list(),
        },
      )
      .await?;
    }
    ClientMessage::AttachSession {
      session,
      resume_from,
    } => match sessions.resolve(&session) {
      Ok(session) => return handle_attach(stream, session, resume_from).await,
      Err(error) => send_session_manager_error(&mut stream, &error).await?,
    },
    ClientMessage::KillSession { session } => match sessions.resolve(&session) {
      Ok(session) => match session.kill() {
        Ok(()) => write_frame(&mut stream, &ServerMessage::Success).await?,
        Err(error) => send_control_error(&mut stream, &error).await?,
      },
      Err(error) => send_session_manager_error(&mut stream, &error).await?,
    },
    _ => {
      send_error(
        &mut stream,
        ErrorCode::InvalidRequest,
        "this message is valid only after attaching to a session",
      )
      .await?;
    }
  }

  Ok(())
}

async fn handle_attach(
  stream: UnixStream,
  session: Arc<Session>,
  resume_from: Option<u64>,
) -> Result<(), ConnectionError> {
  let mut events = session.subscribe();
  let snapshot = match session.snapshot_from(resume_from) {
    Ok(snapshot) => snapshot,
    Err(error) => {
      let mut stream = stream;
      send_journal_error(&mut stream, &error).await?;
      return Ok(());
    }
  };
  let session_info = session.info();
  let (mut reader, mut writer) = stream.into_split();

  write_frame(
    &mut writer,
    &ServerMessage::Attached {
      session: session_info,
      earliest_sequence: snapshot.earliest_sequence,
      next_sequence: snapshot.next_sequence,
      replay_from: snapshot.replay_from,
      history_gap: snapshot.history_gap,
    },
  )
  .await?;

  let mut sent_sequence = snapshot.replay_from;
  for chunk in snapshot.chunks {
    sent_sequence = chunk.sequence_end();
    write_frame(&mut writer, &chunk.into_server_message()).await?;
  }

  loop {
    tokio::select! {
      incoming = read_frame::<_, ClientMessage>(&mut reader) => {
        let Some(message) = incoming? else {
          return Ok(());
        };
        match message {
          ClientMessage::Input { data } => {
            let session = Arc::clone(&session);
            tokio::task::spawn_blocking(move || session.write_input(&data)).await??;
          }
          ClientMessage::Resize { terminal_size } => {
            let session = Arc::clone(&session);
            tokio::task::spawn_blocking(move || session.resize(terminal_size)).await??;
          }
          ClientMessage::Detach => return Ok(()),
          _ => {
            send_error(
              &mut writer,
              ErrorCode::InvalidRequest,
              "only input, resize, and detach are valid while attached",
            )
            .await?;
          }
        }
      }
      event = events.recv() => {
        match event {
          Ok(SessionEvent::Output(chunk)) => {
            if let Some(chunk) = chunk_after(chunk, sent_sequence) {
              sent_sequence = chunk.sequence_end();
              write_frame(&mut writer, &chunk.into_server_message()).await?;
            }
          }
          Ok(SessionEvent::Ended { exit_code }) => {
            write_frame(
              &mut writer,
              &ServerMessage::SessionEnded {
                session_id: session.info().session_id,
                exit_code,
              },
            )
            .await?;
            return Ok(());
          }
          Err(broadcast::error::RecvError::Lagged(_)) => {
            let recovery = session.snapshot_from(Some(sent_sequence))?;
            for chunk in recovery.chunks {
              sent_sequence = chunk.sequence_end();
              write_frame(&mut writer, &chunk.into_server_message()).await?;
            }
          }
          Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
      }
    }
  }
}

fn chunk_after(chunk: OutputChunk, sequence: u64) -> Option<OutputChunk> {
  if chunk.sequence_end() <= sequence {
    return None;
  }
  if chunk.sequence_start >= sequence {
    return Some(chunk);
  }

  let offset = usize::try_from(sequence - chunk.sequence_start).ok()?;
  Some(OutputChunk {
    sequence_start: sequence,
    data: chunk.data[offset..].to_vec(),
  })
}

async fn send_session_manager_error(
  stream: &mut UnixStream,
  error: &SessionManagerError,
) -> Result<(), CodecError> {
  let code = match error {
    SessionManagerError::InvalidName { .. } => ErrorCode::InvalidSessionName,
    SessionManagerError::AlreadyExists { .. } => ErrorCode::SessionAlreadyExists,
    SessionManagerError::NotFound { .. } => ErrorCode::SessionNotFound,
    SessionManagerError::Pty(_)
    | SessionManagerError::Spawn(_)
    | SessionManagerError::ReaderThread(_)
    | SessionManagerError::WaiterThread(_) => ErrorCode::Internal,
  };
  send_error(stream, code, &error.to_string()).await
}

async fn send_journal_error(
  stream: &mut UnixStream,
  error: &JournalError,
) -> Result<(), CodecError> {
  let code = match error {
    JournalError::SequenceAhead { .. } => ErrorCode::SequenceAhead,
  };
  send_error(stream, code, &error.to_string()).await
}

async fn send_control_error(
  stream: &mut UnixStream,
  error: &SessionControlError,
) -> Result<(), CodecError> {
  send_error(stream, ErrorCode::Internal, &error.to_string()).await
}

async fn send_error<W>(writer: &mut W, code: ErrorCode, message: &str) -> Result<(), CodecError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  write_frame(
    writer,
    &ServerMessage::Error {
      code,
      message: message.into(),
    },
  )
  .await
}

#[derive(Debug, Error)]
enum ConnectionError {
  #[error(transparent)]
  Codec(#[from] CodecError),
  #[error(transparent)]
  Journal(#[from] JournalError),
  #[error(transparent)]
  Control(#[from] SessionControlError),
  #[error("blocking daemon task failed: {0}")]
  Task(#[from] tokio::task::JoinError),
}

#[derive(Default)]
struct ConnectionTracker {
  active: AtomicUsize,
  changed: Arc<Notify>,
}

impl ConnectionTracker {
  fn open(self: &Arc<Self>) -> ConnectionGuard {
    self.active.fetch_add(1, Ordering::AcqRel);
    self.changed.notify_one();
    ConnectionGuard {
      tracker: Arc::clone(self),
    }
  }

  fn count(&self) -> usize {
    self.active.load(Ordering::Acquire)
  }

  async fn changed(&self) {
    self.changed.notified().await;
  }
}

struct ConnectionGuard {
  tracker: Arc<ConnectionTracker>,
}

impl Drop for ConnectionGuard {
  fn drop(&mut self) {
    self.tracker.active.fetch_sub(1, Ordering::AcqRel);
    self.tracker.changed.notify_one();
  }
}

struct SocketGuard {
  path: PathBuf,
  device: u64,
  inode: u64,
}

impl SocketGuard {
  fn new(path: PathBuf) -> io::Result<Self> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(&path)?;
    Ok(Self {
      path,
      device: metadata.dev(),
      inode: metadata.ino(),
    })
  }
}

impl Drop for SocketGuard {
  fn drop(&mut self) {
    use std::os::unix::fs::MetadataExt;

    if let Ok(metadata) = std::fs::symlink_metadata(&self.path)
      && metadata.dev() == self.device
      && metadata.ino() == self.inode
    {
      let _ignored = std::fs::remove_file(&self.path);
    }
  }
}
