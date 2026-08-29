use crate::session::{
  AttachSnapshot, Session, SessionControlError, SessionEvent, SessionManager, SessionManagerError,
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
use uuid::Uuid;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct DaemonConfig {
  pub socket_path: PathBuf,
  pub journal_capacity_bytes: usize,
  pub checkpoint_interval_bytes: usize,
  pub startup_idle_timeout: Duration,
}

impl Default for DaemonConfig {
  fn default() -> Self {
    Self {
      socket_path: rmux_ipc::socket_path(),
      journal_capacity_bytes: 4 * 1024 * 1024,
      checkpoint_interval_bytes: 256 * 1024,
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
  let sessions = SessionManager::new(
    config.journal_capacity_bytes,
    config.checkpoint_interval_bytes,
  );
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

  handle_request(stream, sessions).await
}

async fn handle_request(
  mut stream: UnixStream,
  sessions: SessionManager,
) -> Result<(), ConnectionError> {
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
      terminal_size,
      request_input_lease,
      request_layout_lease,
    } => match sessions.resolve(&session) {
      Ok(session) => {
        return handle_attach(
          stream,
          session,
          resume_from,
          terminal_size,
          request_input_lease,
          request_layout_lease,
        )
        .await;
      }
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
  client_terminal_size: rmux_proto::TerminalSize,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> Result<(), ConnectionError> {
  let attachment_id = Uuid::new_v4().to_string();
  let attachment_leases = session.attach(&attachment_id, request_input_lease, request_layout_lease);
  let _attachment_guard = AttachmentGuard {
    session: Arc::clone(&session),
    attachment_id: attachment_id.clone(),
  };

  if attachment_leases.layout.owned_by_client {
    let resize_session = Arc::clone(&session);
    let resize_attachment_id = attachment_id.clone();
    let resize_terminal_size = client_terminal_size.clone();
    match tokio::task::spawn_blocking(move || {
      resize_session.resize(&resize_attachment_id, resize_terminal_size)
    })
    .await?
    {
      Ok(()) => {}
      Err(error) => {
        let mut stream = stream;
        send_control_error(&mut stream, &error).await?;
        return Ok(());
      }
    }
  }

  let mut events = session.subscribe();
  let snapshot = match session.snapshot_for_attach(resume_from) {
    Ok(snapshot) => snapshot,
    Err(error) => {
      let mut stream = stream;
      send_journal_error(&mut stream, &error).await?;
      return Ok(());
    }
  };
  let session_info = session.info();
  let (mut reader, mut writer) = stream.into_split();
  let mut sent_sequence = send_attached(
    &mut writer,
    session_info,
    snapshot,
    client_terminal_size,
    attachment_leases,
  )
  .await?;

  loop {
    tokio::select! {
      incoming = read_frame::<_, ClientMessage>(&mut reader) => {
        let Some(message) = incoming? else {
          return Ok(());
        };
        if !process_attach_input(
          &mut writer,
          Arc::clone(&session),
          &attachment_id,
          message,
        )
        .await?
        {
          return Ok(());
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
            sent_sequence = recover_lag(&mut writer, &session, sent_sequence).await?;
          }
          Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
      }
    }
  }
}

async fn send_attached<W>(
  writer: &mut W,
  session: rmux_proto::SessionInfo,
  snapshot: AttachSnapshot,
  client_terminal_size: rmux_proto::TerminalSize,
  attachment_leases: rmux_core::AttachmentLeases,
) -> Result<u64, ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  let terminal_size_mismatch = session.terminal_size != client_terminal_size;
  write_frame(
    writer,
    &ServerMessage::Attached {
      session,
      earliest_sequence: snapshot.journal.earliest_sequence,
      next_sequence: snapshot.journal.next_sequence,
      replay_from: snapshot.journal.replay_from,
      history_gap: snapshot.history_gap,
      checkpoint: snapshot.checkpoint,
      terminal_size_mismatch,
      input_lease: attachment_leases.input,
      layout_lease: attachment_leases.layout,
    },
  )
  .await?;
  forward_chunks(
    writer,
    snapshot.journal.replay_from,
    snapshot.journal.chunks,
  )
  .await
}

async fn process_attach_input<W>(
  writer: &mut W,
  session: Arc<Session>,
  attachment_id: &str,
  message: ClientMessage,
) -> Result<bool, ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  match message {
    ClientMessage::Input { data } => {
      let attachment_id = attachment_id.to_owned();
      let result =
        tokio::task::spawn_blocking(move || session.write_input(&attachment_id, &data)).await?;
      if let Err(error) = result {
        send_control_error(writer, &error).await?;
      }
    }
    ClientMessage::Resize { terminal_size } => {
      let attachment_id = attachment_id.to_owned();
      let result =
        tokio::task::spawn_blocking(move || session.resize(&attachment_id, terminal_size)).await?;
      if let Err(error) = result {
        send_control_error(writer, &error).await?;
      }
    }
    ClientMessage::AcquireLease { lease } => {
      let status = session.acquire_lease(attachment_id, lease);
      write_frame(writer, &ServerMessage::LeaseStatus { lease, status }).await?;
    }
    ClientMessage::ReleaseLease { lease } => {
      let status = session.release_lease(attachment_id, lease);
      write_frame(writer, &ServerMessage::LeaseStatus { lease, status }).await?;
    }
    ClientMessage::Detach => return Ok(false),
    _ => {
      send_error(
        writer,
        ErrorCode::InvalidRequest,
        "only input, resize, lease control, and detach are valid while attached",
      )
      .await?;
    }
  }
  Ok(true)
}

async fn recover_lag<W>(
  writer: &mut W,
  session: &Session,
  sent_sequence: u64,
) -> Result<u64, ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  let recovery = session.snapshot_for_attach(Some(sent_sequence))?;
  let sent_sequence = if let Some(checkpoint) = recovery.checkpoint {
    let sequence = checkpoint.sequence;
    write_frame(
      writer,
      &ServerMessage::Checkpoint {
        checkpoint,
        history_gap: recovery.history_gap,
      },
    )
    .await?;
    sequence
  } else {
    sent_sequence
  };
  forward_chunks(writer, sent_sequence, recovery.journal.chunks).await
}

async fn forward_chunks<W>(
  writer: &mut W,
  mut sent_sequence: u64,
  chunks: Vec<OutputChunk>,
) -> Result<u64, ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  for chunk in chunks {
    sent_sequence = chunk.sequence_end();
    write_frame(writer, &chunk.into_server_message()).await?;
  }
  Ok(sent_sequence)
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

async fn send_control_error<W>(
  writer: &mut W,
  error: &SessionControlError,
) -> Result<(), CodecError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  let code = match error {
    SessionControlError::InputLeaseRequired => ErrorCode::InputLeaseRequired,
    SessionControlError::LayoutLeaseRequired => ErrorCode::LayoutLeaseRequired,
    SessionControlError::Io(_) | SessionControlError::Pty(_) => ErrorCode::Internal,
  };
  send_error(writer, code, &error.to_string()).await
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

struct AttachmentGuard {
  session: Arc<Session>,
  attachment_id: String,
}

impl Drop for AttachmentGuard {
  fn drop(&mut self) {
    self.session.release_attachment(&self.attachment_id);
  }
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
