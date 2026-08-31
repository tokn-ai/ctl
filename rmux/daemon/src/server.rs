use crate::session::{
  AttachSnapshot, Session, SessionControlError, SessionEvent, SessionManager, SessionManagerError,
};
use rmux_core::{JournalError, OutputChunk};
use rmux_proto::{
  ClientMessage, CodecError, ErrorCode, LeaseKind, PROTOCOL_VERSION, ServerMessage, ShellState,
  read_frame, write_frame,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, broadcast, watch};
use tokio::time::{Instant, sleep_until, timeout_at};
use uuid::Uuid;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MIN_ATTACHMENT_LIVENESS_TIMEOUT: Duration = Duration::from_millis(100);
pub const MAX_ATTACHMENT_LIVENESS_TIMEOUT: Duration = Duration::from_mins(5);
pub const DEFAULT_ATTACHMENT_LIVENESS_TIMEOUT: Duration = Duration::from_secs(30);
// An attachment cannot prove liveness before initial delivery: the client
// learns the heartbeat cadence from `attached`, while rmuxd serially sends the
// replay before it can process queued client frames. Give that transfer a
// separate, finite window instead of applying post-attach liveness too early.
const INITIAL_ATTACHMENT_DELIVERY_TIMEOUT: Duration = Duration::from_mins(5);

#[derive(Debug, Clone)]
pub struct DaemonConfig {
  pub socket_path: PathBuf,
  pub journal_capacity_bytes: usize,
  pub checkpoint_interval_bytes: usize,
  pub startup_idle_timeout: Duration,
  /// Maximum time an attached client may remain silent before `rmuxd` releases
  /// its connection-bound leases.
  pub attachment_liveness_timeout: Duration,
}

impl Default for DaemonConfig {
  fn default() -> Self {
    Self {
      socket_path: rmux_ipc::socket_path(),
      journal_capacity_bytes: 4 * 1024 * 1024,
      checkpoint_interval_bytes: 256 * 1024,
      startup_idle_timeout: Duration::from_secs(10),
      attachment_liveness_timeout: DEFAULT_ATTACHMENT_LIVENESS_TIMEOUT,
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
  #[error("attachment liveness timeout {actual:?} must be between {minimum:?} and {maximum:?}")]
  InvalidAttachmentLivenessTimeout {
    actual: Duration,
    minimum: Duration,
    maximum: Duration,
  },
  #[error("could not lock local endpoint startup at {path}: {source}")]
  EndpointStartupLock { path: PathBuf, source: io::Error },
}

#[derive(Debug, Clone, Copy)]
struct AttachmentLiveness {
  timeout: Duration,
  timeout_ms: u64,
  heartbeat_interval_ms: u64,
}

fn attachment_liveness(timeout: Duration) -> Result<AttachmentLiveness, DaemonError> {
  if !(MIN_ATTACHMENT_LIVENESS_TIMEOUT..=MAX_ATTACHMENT_LIVENESS_TIMEOUT).contains(&timeout) {
    return Err(DaemonError::InvalidAttachmentLivenessTimeout {
      actual: timeout,
      minimum: MIN_ATTACHMENT_LIVENESS_TIMEOUT,
      maximum: MAX_ATTACHMENT_LIVENESS_TIMEOUT,
    });
  }

  let timeout_ms = u64::try_from(timeout.as_millis()).expect("bounded timeout fits in u64");
  Ok(AttachmentLiveness {
    timeout,
    timeout_ms,
    heartbeat_interval_ms: (timeout_ms / 3).max(1),
  })
}

/// Runs the daemon until its final session exits or an interrupt is received.
///
/// # Errors
///
/// Returns an error when the runtime directory or socket cannot be prepared,
/// the endpoint is already served, or the accept loop fails.
pub async fn run(config: DaemonConfig) -> Result<(), DaemonError> {
  let attachment_liveness = attachment_liveness(config.attachment_liveness_timeout)?;
  rmux_ipc::prepare_runtime_directory(&config.socket_path)
    .map_err(DaemonError::RuntimeDirectory)?;
  let runtime_directory = config.socket_path.parent().ok_or_else(|| {
    DaemonError::RuntimeDirectory(io::Error::new(
      io::ErrorKind::InvalidInput,
      format!(
        "socket path {} has no runtime directory",
        config.socket_path.display()
      ),
    ))
  })?;
  let endpoint_startup_lock =
    EndpointStartupLock::acquire(&config.socket_path).map_err(|source| {
      DaemonError::EndpointStartupLock {
        path: endpoint_startup_lock_path(&config.socket_path),
        source,
      }
    })?;
  let listener = bind_listener(&config.socket_path).await?;
  let _socket_guard = SocketGuard::new(config.socket_path.clone())?;
  drop(endpoint_startup_lock);
  let sessions = SessionManager::new(
    runtime_directory.to_path_buf(),
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
          if let Err(error) = handle_connection(stream, sessions, attachment_liveness).await {
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
  attachment_liveness: AttachmentLiveness,
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
      heartbeat_interval_ms: attachment_liveness.heartbeat_interval_ms,
      attachment_liveness_timeout_ms: attachment_liveness.timeout_ms,
    },
  )
  .await?;

  handle_request(stream, sessions, attachment_liveness.timeout).await
}

async fn handle_request(
  mut stream: UnixStream,
  sessions: SessionManager,
  attachment_liveness_timeout: Duration,
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
    ClientMessage::GetShellState { session } => match sessions.resolve(&session) {
      Ok(session) => {
        write_frame(
          &mut stream,
          &ServerMessage::ShellStateResponse {
            session: session.info(),
            shell_state: session.shell_state_for_inspection(),
          },
        )
        .await?;
      }
      Err(error) => send_session_manager_error(&mut stream, &error).await?,
    },
    ClientMessage::AttachSession {
      session,
      resume_from,
      terminal_size,
      request_input_lease,
      request_layout_lease,
      request_command_line,
    } => match sessions.resolve(&session) {
      Ok(session) => {
        let request = AttachParameters {
          resume_from,
          client_terminal_size: terminal_size,
          request_input_lease,
          request_layout_lease,
          request_command_line,
          attachment_liveness_timeout,
        };
        return handle_attach(stream, session, request).await;
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

struct AttachParameters {
  resume_from: Option<u64>,
  client_terminal_size: rmux_proto::TerminalSize,
  request_input_lease: bool,
  request_layout_lease: bool,
  request_command_line: bool,
  attachment_liveness_timeout: Duration,
}

async fn handle_attach(
  mut stream: UnixStream,
  session: Arc<Session>,
  request: AttachParameters,
) -> Result<(), ConnectionError> {
  let attachment_id = Uuid::new_v4().to_string();
  let attachment_leases = session.attach(
    &attachment_id,
    request.request_input_lease,
    request.request_layout_lease,
  );
  let _attachment_guard = AttachmentGuard {
    session: Arc::clone(&session),
    attachment_id: attachment_id.clone(),
  };
  let initial_delivery_deadline = initial_attachment_delivery_deadline();

  if attachment_leases.layout.owned_by_client
    && !apply_initial_resize(
      &mut stream,
      Arc::clone(&session),
      attachment_id.clone(),
      request.client_terminal_size.clone(),
      initial_delivery_deadline,
    )
    .await?
  {
    return Ok(());
  }

  let events = session.subscribe();
  let shell_state_updates = session.subscribe_shell_state();
  let Some(snapshot) = take_initial_snapshot(
    &mut stream,
    Arc::clone(&session),
    request.resume_from,
    initial_delivery_deadline,
  )
  .await?
  else {
    return Ok(());
  };
  let initial_shell_state = shell_state_for_attachment(
    snapshot.shell_state.clone(),
    request.request_command_line && attachment_leases.input.owned_by_client,
  );
  let checkpoint_geometry_revision = snapshot.checkpoint_geometry_revision;
  let (reader, mut writer) = stream.into_split();
  let sent_sequence = match timeout_at(
    initial_delivery_deadline,
    send_attached(
      &mut writer,
      snapshot,
      request.client_terminal_size,
      attachment_leases,
      initial_shell_state.clone(),
    ),
  )
  .await
  {
    Ok(result) => result?,
    Err(_) => return Ok(()),
  };

  let attachment = LiveAttachment {
    reader,
    writer,
    events,
    shell_state_updates,
    sent_sequence,
    checkpoint_geometry_revision,
    shell_state_revision: initial_shell_state.revision,
    request_command_line: request.request_command_line,
  };
  drive_attachment(
    attachment,
    session,
    attachment_id,
    request.attachment_liveness_timeout,
    Instant::now() + request.attachment_liveness_timeout,
  )
  .await
}

async fn apply_initial_resize(
  stream: &mut UnixStream,
  session: Arc<Session>,
  attachment_id: String,
  terminal_size: rmux_proto::TerminalSize,
  deadline: Instant,
) -> Result<bool, ConnectionError> {
  let resize = tokio::task::spawn_blocking(move || session.resize(&attachment_id, terminal_size));
  match timeout_at(deadline, resize).await {
    Err(_) => Ok(false),
    Ok(result) => match result? {
      Ok(()) => Ok(true),
      Err(error) => match timeout_at(deadline, send_control_error(stream, &error)).await {
        Ok(result) => {
          result?;
          Ok(false)
        }
        Err(_) => Ok(false),
      },
    },
  }
}

async fn take_initial_snapshot(
  stream: &mut UnixStream,
  session: Arc<Session>,
  resume_from: Option<u64>,
  deadline: Instant,
) -> Result<Option<AttachSnapshot>, ConnectionError> {
  let snapshot = tokio::task::spawn_blocking(move || session.snapshot_for_attach(resume_from));
  match timeout_at(deadline, snapshot).await {
    Err(_) => Ok(None),
    Ok(result) => match result? {
      Ok(snapshot) => Ok(Some(snapshot)),
      Err(error) => match timeout_at(deadline, send_journal_error(stream, &error)).await {
        Ok(result) => {
          result?;
          Ok(None)
        }
        Err(_) => Ok(None),
      },
    },
  }
}

struct LiveAttachment {
  reader: OwnedReadHalf,
  writer: OwnedWriteHalf,
  events: broadcast::Receiver<SessionEvent>,
  shell_state_updates: watch::Receiver<ShellState>,
  sent_sequence: u64,
  /// Internal ordering for geometry changes represented by the last checkpoint.
  checkpoint_geometry_revision: Option<u64>,
  shell_state_revision: u64,
  request_command_line: bool,
}

async fn drive_attachment(
  attachment: LiveAttachment,
  session: Arc<Session>,
  attachment_id: String,
  attachment_liveness_timeout: Duration,
  deadline: Instant,
) -> Result<(), ConnectionError> {
  let mut driver = AttachmentDriver {
    attachment,
    session,
    attachment_id,
    attachment_liveness_timeout,
    deadline,
  };
  loop {
    tokio::select! {
      biased;
      () = sleep_until(driver.deadline) => return Ok(()),
      incoming = read_frame::<_, ClientMessage>(&mut driver.attachment.reader) => {
        let Some(message) = incoming? else {
          return Ok(());
        };
        if !driver.process_client_message(message).await? {
          return Ok(());
        }
      }
      event = driver.attachment.events.recv() => {
        if !driver.process_session_event(event).await? {
          return Ok(());
        }
      }
      // Raw PTY output is canonical and must take precedence over advisory
      // metadata. A watch receiver coalesces state updates, so delaying one
      // here never loses the latest snapshot.
      changed = driver.attachment.shell_state_updates.changed() => {
        if !driver.process_shell_state_update(changed).await? {
          return Ok(());
        }
      }
    }
  }
}

struct AttachmentDriver {
  attachment: LiveAttachment,
  session: Arc<Session>,
  attachment_id: String,
  attachment_liveness_timeout: Duration,
  deadline: Instant,
}

impl AttachmentDriver {
  async fn process_client_message(
    &mut self,
    message: ClientMessage,
  ) -> Result<bool, ConnectionError> {
    if Instant::now() >= self.deadline {
      return Ok(false);
    }
    if renews_attachment_liveness(&message) {
      self.deadline = Instant::now() + self.attachment_liveness_timeout;
    }
    match timeout_at(
      self.deadline,
      process_attach_input(
        &mut self.attachment.writer,
        Arc::clone(&self.session),
        &self.attachment_id,
        self.attachment.request_command_line,
        message,
      ),
    )
    .await
    {
      Ok(result) => result,
      Err(_) => Ok(false),
    }
  }

  async fn process_session_event(
    &mut self,
    event: Result<SessionEvent, broadcast::error::RecvError>,
  ) -> Result<bool, ConnectionError> {
    match event {
      Ok(SessionEvent::Output(chunk)) => {
        if let Some(chunk) = chunk_after(chunk, self.attachment.sent_sequence) {
          self.attachment.sent_sequence = chunk.sequence_end();
          return write_before_deadline(
            &mut self.attachment.writer,
            &chunk.into_server_message(),
            self.deadline,
          )
          .await
          .map(|written| written.is_some());
        }
        Ok(true)
      }
      Ok(SessionEvent::PtyGeometryChanged {
        terminal_size,
        observed_sequence,
        geometry_revision,
      }) => {
        self
          .process_geometry_change(terminal_size, observed_sequence, geometry_revision)
          .await
      }
      Ok(SessionEvent::Ended { exit_code }) => {
        let shell_state = self
          .session
          .shell_state_for_attachment(&self.attachment_id, self.attachment.request_command_line);
        if write_shell_state_snapshot(
          &mut self.attachment.writer,
          shell_state,
          &mut self.attachment.shell_state_revision,
          false,
          self.deadline,
        )
        .await?
        .is_none()
        {
          return Ok(false);
        }
        let message = ServerMessage::SessionEnded {
          session_id: self.session.info().session_id,
          exit_code,
        };
        return write_before_deadline(&mut self.attachment.writer, &message, self.deadline)
          .await
          .map(|_| false);
      }
      Err(broadcast::error::RecvError::Lagged(_)) => {
        let recovery = match timeout_at(
          self.deadline,
          recover_lag(
            &mut self.attachment.writer,
            &self.session,
            self.attachment.sent_sequence,
          ),
        )
        .await
        {
          Ok(result) => result?,
          Err(_) => return Ok(false),
        };
        self.attachment.sent_sequence = recovery.sent_sequence;
        if let Some(checkpoint_geometry_revision) = recovery.checkpoint_geometry_revision {
          self.attachment.checkpoint_geometry_revision = Some(checkpoint_geometry_revision);
        }
        let shell_state = self
          .session
          .shell_state_for_attachment(&self.attachment_id, self.attachment.request_command_line);
        if write_shell_state_snapshot(
          &mut self.attachment.writer,
          shell_state,
          &mut self.attachment.shell_state_revision,
          true,
          self.deadline,
        )
        .await?
        .is_none()
        {
          return Ok(false);
        }
        Ok(true)
      }
      Err(broadcast::error::RecvError::Closed) => Ok(false),
    }
  }

  async fn process_geometry_change(
    &mut self,
    terminal_size: rmux_proto::TerminalSize,
    observed_sequence: u64,
    geometry_revision: u64,
  ) -> Result<bool, ConnectionError> {
    if geometry_event_is_stale(
      self.attachment.checkpoint_geometry_revision,
      self.attachment.sent_sequence,
      observed_sequence,
      geometry_revision,
    ) {
      return Ok(true);
    }

    // `Session::resize` serializes this event with output publication, so a
    // live attachment can only see it at its next raw-output offset. Treat an
    // impossible gap as a recovery boundary instead of allowing a renderer to
    // resize ahead of unrendered raw output.
    if observed_sequence > self.attachment.sent_sequence {
      let recovery = match timeout_at(
        self.deadline,
        recover_lag(
          &mut self.attachment.writer,
          &self.session,
          self.attachment.sent_sequence,
        ),
      )
      .await
      {
        Ok(result) => result?,
        Err(_) => return Ok(false),
      };
      self.attachment.sent_sequence = recovery.sent_sequence;
      if let Some(checkpoint_geometry_revision) = recovery.checkpoint_geometry_revision {
        self.attachment.checkpoint_geometry_revision = Some(checkpoint_geometry_revision);
      }
      return Ok(true);
    }

    let message = ServerMessage::PtyGeometryChanged {
      terminal_size,
      observed_sequence,
    };
    write_before_deadline(&mut self.attachment.writer, &message, self.deadline)
      .await
      .map(|written| written.is_some())
  }

  async fn process_shell_state_update(
    &mut self,
    changed: Result<(), watch::error::RecvError>,
  ) -> Result<bool, ConnectionError> {
    if changed.is_err() {
      return Ok(false);
    }
    let shell_state = self
      .attachment
      .shell_state_updates
      .borrow_and_update()
      .clone();
    if shell_state.revision <= self.attachment.shell_state_revision {
      return Ok(true);
    }
    let shell_state = shell_state_for_attachment(
      shell_state,
      self.attachment.request_command_line && self.session.owns_input_lease(&self.attachment_id),
    );
    write_shell_state_snapshot(
      &mut self.attachment.writer,
      shell_state,
      &mut self.attachment.shell_state_revision,
      false,
      self.deadline,
    )
    .await
    .map(|written| written.is_some())
  }
}

async fn write_shell_state_snapshot<W>(
  writer: &mut W,
  shell_state: ShellState,
  last_revision: &mut u64,
  force: bool,
  deadline: Instant,
) -> Result<Option<()>, ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  if !force && shell_state.revision <= *last_revision {
    return Ok(Some(()));
  }
  *last_revision = (*last_revision).max(shell_state.revision);
  write_before_deadline(
    writer,
    &ServerMessage::ShellStateChanged { state: shell_state },
    deadline,
  )
  .await
}

async fn write_before_deadline<W>(
  writer: &mut W,
  message: &ServerMessage,
  deadline: Instant,
) -> Result<Option<()>, ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  match timeout_at(deadline, write_frame(writer, message)).await {
    Ok(result) => {
      result?;
      Ok(Some(()))
    }
    Err(_) => Ok(None),
  }
}

async fn send_attached<W>(
  writer: &mut W,
  snapshot: AttachSnapshot,
  client_terminal_size: rmux_proto::TerminalSize,
  attachment_leases: rmux_core::AttachmentLeases,
  shell_state: ShellState,
) -> Result<u64, ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  let terminal_size_mismatch = snapshot.session.terminal_size != client_terminal_size;
  write_frame(
    writer,
    &ServerMessage::Attached {
      session: snapshot.session,
      earliest_sequence: snapshot.journal.earliest_sequence,
      next_sequence: snapshot.journal.next_sequence,
      replay_from: snapshot.journal.replay_from,
      history_gap: snapshot.history_gap,
      checkpoint: snapshot.checkpoint,
      terminal_size_mismatch,
      input_lease: attachment_leases.input,
      layout_lease: attachment_leases.layout,
      shell_state,
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

fn shell_state_for_attachment(
  mut shell_state: ShellState,
  may_view_command_line: bool,
) -> ShellState {
  shell_state.command_line_redacted = false;
  if !may_view_command_line && shell_state.current_command_line.is_some() {
    shell_state.current_command_line = None;
    shell_state.command_line_redacted = true;
  }
  shell_state
}

async fn process_attach_input<W>(
  writer: &mut W,
  session: Arc<Session>,
  attachment_id: &str,
  request_command_line: bool,
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
      let already_owned_input =
        lease == LeaseKind::Input && session.owns_input_lease(attachment_id);
      let status = session.acquire_lease(attachment_id, lease);
      let acquired_input = status.owned_by_client;
      write_frame(writer, &ServerMessage::LeaseStatus { lease, status }).await?;
      if lease == LeaseKind::Input && request_command_line && !already_owned_input && acquired_input
      {
        session.refresh_shell_state_for_visibility();
      }
    }
    ClientMessage::ReleaseLease { lease } => {
      let status = session.release_lease(attachment_id, lease);
      write_frame(writer, &ServerMessage::LeaseStatus { lease, status }).await?;
    }
    ClientMessage::Heartbeat { nonce } => {
      write_frame(writer, &ServerMessage::HeartbeatAck { nonce }).await?;
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

fn renews_attachment_liveness(message: &ClientMessage) -> bool {
  matches!(
    message,
    ClientMessage::Input { .. }
      | ClientMessage::Resize { .. }
      | ClientMessage::AcquireLease { .. }
      | ClientMessage::ReleaseLease { .. }
      | ClientMessage::Heartbeat { .. }
      | ClientMessage::Detach
  )
}

struct RecoveryResult {
  sent_sequence: u64,
  checkpoint_geometry_revision: Option<u64>,
}

async fn recover_lag<W>(
  writer: &mut W,
  session: &Session,
  sent_sequence: u64,
) -> Result<RecoveryResult, ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  let recovery = session.snapshot_for_attach(Some(sent_sequence))?;
  let checkpoint_geometry_revision = recovery.checkpoint_geometry_revision;
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
  let sent_sequence = forward_chunks(writer, sent_sequence, recovery.journal.chunks).await?;
  Ok(RecoveryResult {
    sent_sequence,
    checkpoint_geometry_revision,
  })
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

fn initial_attachment_delivery_deadline() -> Instant {
  Instant::now() + INITIAL_ATTACHMENT_DELIVERY_TIMEOUT
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

fn geometry_event_is_stale(
  checkpoint_geometry_revision: Option<u64>,
  sent_sequence: u64,
  observed_sequence: u64,
  geometry_revision: u64,
) -> bool {
  checkpoint_geometry_revision
    .is_some_and(|checkpoint_revision| geometry_revision <= checkpoint_revision)
    || observed_sequence < sent_sequence
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
    | SessionManagerError::ShellReporter(_)
    | SessionManagerError::ReaderThread(_)
    | SessionManagerError::WaiterThread(_)
    | SessionManagerError::AutomaticNameExhausted => ErrorCode::Internal,
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

struct EndpointStartupLock {
  _file: std::fs::File,
}

impl EndpointStartupLock {
  fn acquire(socket_path: &Path) -> io::Result<Self> {
    use rustix::fs::{CWD, FlockOperation, Mode, OFlags, fchmod, flock, openat};

    let path = endpoint_startup_lock_path(socket_path);
    let file = std::fs::File::from(openat(
      CWD,
      &path,
      OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::RDWR,
      Mode::RUSR | Mode::WUSR,
    )?);
    flock(&file, FlockOperation::LockExclusive)?;
    fchmod(&file, Mode::RUSR | Mode::WUSR)?;
    Ok(Self { _file: file })
  }
}

fn endpoint_startup_lock_path(socket_path: &Path) -> PathBuf {
  let mut path = socket_path.as_os_str().to_os_string();
  path.push(".lock");
  path.into()
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

#[cfg(test)]
mod tests {
  use super::*;
  use rmux_core::JournalSnapshot;
  use rmux_proto::{LeaseStatus, SessionStatus};
  use tokio::time::{sleep, timeout};

  #[test]
  fn endpoint_startup_lock_is_exclusive_and_owner_only() {
    use rustix::fs::{FlockOperation, flock};
    use std::os::unix::fs::PermissionsExt as _;

    let directory = std::env::temp_dir().join(format!("rmux-start-lock-{}", Uuid::new_v4()));
    let cleanup = TestDirectory(directory.clone());
    let socket_path = directory.join("rmux.sock");
    rmux_ipc::prepare_runtime_directory(&socket_path).unwrap();

    let first = EndpointStartupLock::acquire(&socket_path).unwrap();
    let lock_path = endpoint_startup_lock_path(&socket_path);
    let second = std::fs::OpenOptions::new()
      .read(true)
      .write(true)
      .open(&lock_path)
      .unwrap();
    let error = flock(&second, FlockOperation::NonBlockingLockExclusive).unwrap_err();
    assert!(error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::WOULDBLOCK);
    assert_eq!(
      std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
      0o600
    );

    drop(first);
    flock(&second, FlockOperation::LockExclusive).unwrap();
    drop(second);
    drop(cleanup);
  }

  struct TestDirectory(PathBuf);

  impl Drop for TestDirectory {
    fn drop(&mut self) {
      let _ignored = std::fs::remove_dir_all(&self.0);
    }
  }

  #[tokio::test]
  async fn initial_delivery_window_outlasts_post_attach_liveness() {
    let liveness_timeout = Duration::from_millis(25);
    let replay = vec![b'x'; 4 * 1024];
    let next_sequence = u64::try_from(replay.len()).expect("test replay fits in u64");
    let snapshot = AttachSnapshot {
      session: rmux_proto::SessionInfo {
        session_id: "session-id".into(),
        name: "session".into(),
        status: SessionStatus::Running,
        created_at_ms: 0,
        next_sequence,
        terminal_size: rmux_proto::TerminalSize::default(),
      },
      checkpoint: None,
      checkpoint_geometry_revision: None,
      journal: JournalSnapshot {
        earliest_sequence: 0,
        next_sequence,
        replay_from: 0,
        history_gap: false,
        chunks: vec![OutputChunk {
          sequence_start: 0,
          data: replay,
        }],
      },
      history_gap: false,
      shell_state: ShellState::default(),
    };
    let attachment_leases = rmux_core::AttachmentLeases {
      input: LeaseStatus {
        held: true,
        owned_by_client: true,
      },
      layout: LeaseStatus {
        held: false,
        owned_by_client: false,
      },
    };
    let (mut writer, mut reader) = tokio::io::duplex(128);
    let delivery = tokio::spawn(async move {
      timeout_at(
        initial_attachment_delivery_deadline(),
        send_attached(
          &mut writer,
          snapshot,
          rmux_proto::TerminalSize::default(),
          attachment_leases,
          ShellState::default(),
        ),
      )
      .await
    });

    // The small duplex buffer prevents the initial replay from completing.
    // It remains in the distinct delivery phase beyond the time at which the
    // normal attachment liveness deadline would otherwise expire.
    sleep(liveness_timeout * 2).await;
    assert!(!delivery.is_finished());

    let attached: ServerMessage = timeout(Duration::from_secs(1), read_frame(&mut reader))
      .await
      .expect("initial attached reply did not arrive")
      .expect("initial attached reply failed")
      .expect("initial attached reply ended unexpectedly");
    let ServerMessage::Attached {
      session,
      next_sequence: attached_next_sequence,
      ..
    } = attached
    else {
      panic!("expected attached response");
    };
    assert_eq!(session.next_sequence, next_sequence);
    assert_eq!(attached_next_sequence, next_sequence);
    let output: ServerMessage = timeout(Duration::from_secs(1), read_frame(&mut reader))
      .await
      .expect("initial replay did not arrive")
      .expect("initial replay failed")
      .expect("initial replay ended unexpectedly");
    assert!(matches!(
      output,
      ServerMessage::Output {
        sequence_start: 0,
        sequence_end,
        ..
      } if sequence_end == next_sequence
    ));

    let sent_sequence = timeout(Duration::from_secs(1), delivery)
      .await
      .expect("initial delivery did not finish")
      .expect("initial delivery task panicked")
      .expect("initial delivery hit its deadline")
      .expect("initial delivery failed");
    assert_eq!(sent_sequence, next_sequence);
  }

  #[test]
  fn checkpoint_suppresses_queued_geometry_but_not_a_later_same_boundary_resize() {
    // A subscription starts before its initial snapshot. The first event was
    // already represented in that snapshot, even though it has the same raw
    // boundary as the later live transition.
    assert!(geometry_event_is_stale(Some(7), 42, 42, 7));
    assert!(!geometry_event_is_stale(Some(7), 42, 42, 8));

    // Once raw output has advanced beyond a transition, a queued older event
    // is also stale for an attachment that resumed without a checkpoint.
    assert!(geometry_event_is_stale(None, 43, 42, 8));
  }
}
