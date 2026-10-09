use crate::history_snapshot::PhysicalHistory;
use crate::session::{
  AttachSnapshot, AttachmentRegistration, PaneMoveError, SessionControlError, SessionEvent,
  SessionManager, SessionManagerError, Terminal,
};
use ctmux_core::{JournalError, OutputChunk};
use ctmux_ipc::{
  LOCAL_CONTROL_SUPPORTED_PROTOCOL_VERSIONS, LocalControlClientMessage, LocalControlErrorCode,
  LocalControlServerMessage, read_local_control_frame, write_local_control_frame,
};
use ctmux_proto::{
  ClientMessage, CodecError, ErrorCode, FrameReader, LeaseKind, MAX_HISTORY_PAGE_BYTES,
  PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, ServerMessage, ShellState,
  TerminalHistoryManifest, TerminalHistorySnapshot, read_frame, write_frame,
};
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
type OwnedReadHalf = tokio::io::ReadHalf<Stream>;
type OwnedWriteHalf = tokio::io::WriteHalf<Stream>;
use ctmux_ipc::Stream;
#[cfg(windows)]
use ctmux_ipc::windows::Listener;
#[cfg(unix)]
use tokio::net::UnixListener as Listener;
use tokio::sync::{Notify, broadcast, watch};
use tokio::time::{Instant, sleep_until, timeout_at};
#[cfg(test)]
#[cfg(all(test, unix))]
use uuid::Uuid;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MIN_ATTACHMENT_LIVENESS_TIMEOUT: Duration = Duration::from_millis(100);
pub const MAX_ATTACHMENT_LIVENESS_TIMEOUT: Duration = Duration::from_mins(5);
pub const DEFAULT_ATTACHMENT_LIVENESS_TIMEOUT: Duration = Duration::from_secs(30);
// An attachment cannot prove liveness before receiving `attached`, because
// that response carries its token, checkpoint, leases, and heartbeat cadence.
// Bound this admission write separately from post-attach liveness.
const INITIAL_ATTACHMENT_DELIVERY_TIMEOUT: Duration = Duration::from_mins(5);
const MAX_PRESENTATION_WINDOW_BYTES: u64 = 8 * 1024 * 1024;
const MIN_OUTPUT_FRAME_CHARGE_BYTES: u64 = 4 * 1024;
const MAX_OUTPUT_FRAME_BYTES: usize = 64 * 1024;
const HISTORY_SNAPSHOT_IDLE_TTL: Duration = Duration::from_mins(2);
const MAX_RECENT_HISTORY_BYTES: usize = 16 * 1024;
const MAX_RECENT_HISTORY_LINES: usize = 64;
const LOCAL_CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
// A GUI retains an already-handshaken control stream while it waits for its
// active attachment to detach (currently up to five seconds). Keep that
// armed operation alive long enough for a serialized window transition, but
// still bound it. Once any restart is accepted, all other control handlers
// are canceled so this longer pre-restart window cannot delay draining.
const LOCAL_CONTROL_ARMED_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_DRAINING_MESSAGE: &str = "ctmuxd is draining for a cooperative restart";

#[derive(Debug, Clone)]
pub struct DaemonConfig {
  pub socket_path: PathBuf,
  pub journal_capacity_bytes: usize,
  pub checkpoint_interval_bytes: usize,
  pub startup_idle_timeout: Duration,
  /// Maximum silence before a transport expires, and the reconnect grace used
  /// after an unexpected transport loss.
  pub attachment_liveness_timeout: Duration,
}

impl Default for DaemonConfig {
  fn default() -> Self {
    Self {
      socket_path: ctmux_ipc::socket_path(),
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
  #[error("another ctmuxd is already listening at {0}")]
  AlreadyRunning(PathBuf),
  #[error("could not bind local socket {path}: {source}")]
  Bind { path: PathBuf, source: io::Error },
  #[error("could not derive local-control socket from {path}: {source}")]
  ControlSocketPath { path: PathBuf, source: io::Error },
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

impl DaemonError {
  /// Classify startup/runtime failure without exposing endpoint paths or raw errors.
  #[must_use]
  pub fn diagnostic(&self) -> (&'static str, Option<i32>) {
    match self {
      Self::RuntimeDirectory(error) => ("daemon_runtime_directory_failed", error.raw_os_error()),
      Self::AlreadyRunning(_) => ("daemon_already_running", None),
      Self::Bind { source, .. } => ("daemon_bind_failed", source.raw_os_error()),
      Self::ControlSocketPath { source, .. } => {
        ("daemon_control_endpoint_failed", source.raw_os_error())
      }
      Self::Io(error) => ("daemon_io_failed", error.raw_os_error()),
      Self::InvalidAttachmentLivenessTimeout { .. } => ("attachment_timeout_invalid", None),
      Self::EndpointStartupLock { source, .. } => {
        ("daemon_startup_lock_failed", source.raw_os_error())
      }
    }
  }
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
  let (endpoints, sessions) = prepare_daemon(&config).await?;
  let DaemonEndpoints {
    listener,
    control_listener,
    #[cfg(unix)]
    _data_socket_guard,
    #[cfg(unix)]
    _control_socket_guard,
  } = endpoints;
  let (connections, restart) = daemon_runtime();
  let startup_deadline = sleep_until(Instant::now() + config.startup_idle_timeout);
  tokio::pin!(startup_deadline);

  loop {
    if (sessions.ever_had_session() || restart.is_draining())
      && sessions.session_count() == 0
      && connections.count() == 0
    {
      return Ok(());
    }

    tokio::select! {
      accepted = listener.accept() => {
        let stream = accepted?.0;
        let sessions = sessions.clone();
        let restart = Arc::clone(&restart);
        let data_connection_shutdown = restart.data_connection_shutdown_receiver();
        let connection_guard = connections.open();
        tokio::spawn(async move {
          let _recorded_result = handle_connection(
            stream,
            sessions,
            restart,
            data_connection_shutdown,
            attachment_liveness,
          )
          .await;
          drop(connection_guard);
        });
      }
      accepted = control_listener.accept() => {
        let stream = accepted?.0;
        let sessions = sessions.clone();
        let restart = Arc::clone(&restart);
        let control_connection_shutdown = restart.control_connection_shutdown_receiver();
        let connection_guard = connections.open();
        tokio::spawn(async move {
          let _recorded_result = handle_local_control_connection(
            stream,
            sessions,
            restart,
            control_connection_shutdown,
          )
          .await;
          drop(connection_guard);
        });
      }
      () = sessions.changed() => {}
      () = connections.changed() => {}
      () = restart.changed() => {}
      () = &mut startup_deadline, if !sessions.ever_had_session() && !restart.is_draining() => {
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

#[cfg_attr(
  windows,
  allow(
    clippy::unused_async,
    reason = "Unix endpoint startup probes existing listeners asynchronously"
  )
)]
async fn prepare_daemon(
  config: &DaemonConfig,
) -> Result<(DaemonEndpoints, SessionManager), DaemonError> {
  #[cfg(unix)]
  ctmux_ipc::prepare_runtime_directory(&config.socket_path)
    .map_err(DaemonError::RuntimeDirectory)?;
  let control_socket_path =
    ctmux_ipc::control_socket_path(&config.socket_path).map_err(|source| {
      DaemonError::ControlSocketPath {
        path: config.socket_path.clone(),
        source,
      }
    })?;
  #[cfg(unix)]
  let runtime_directory = config
    .socket_path
    .parent()
    .map(Path::to_path_buf)
    .ok_or_else(|| {
      DaemonError::RuntimeDirectory(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
          "socket path {} has no runtime directory",
          config.socket_path.display()
        ),
      ))
    })?;
  let endpoints = {
    #[cfg(unix)]
    {
      bind_daemon_endpoints(&config.socket_path, &control_socket_path).await?
    }
    #[cfg(windows)]
    {
      bind_daemon_endpoints(&config.socket_path, &control_socket_path)?
    }
  };
  #[cfg(windows)]
  let runtime_directory = ctmux_ipc::runtime_directory();
  let sessions = SessionManager::new(
    runtime_directory,
    config.journal_capacity_bytes,
    config.checkpoint_interval_bytes,
  );
  Ok((endpoints, sessions))
}

fn daemon_runtime() -> (Arc<ConnectionTracker>, Arc<RestartCoordinator>) {
  (
    Arc::new(ConnectionTracker::default()),
    Arc::new(RestartCoordinator::new()),
  )
}

struct DaemonEndpoints {
  listener: Listener,
  control_listener: Listener,
  #[cfg(unix)]
  _data_socket_guard: SocketGuard,
  #[cfg(unix)]
  _control_socket_guard: SocketGuard,
}

/// Binds both paired daemon endpoints under one startup lock.
///
/// Keeping the lock until both listeners are live prevents a concurrent
/// launcher from observing only the data endpoint and treating the daemon as
/// a legacy instance without local-control support.
#[cfg(unix)]
async fn bind_daemon_endpoints(
  data_socket_path: &Path,
  control_socket_path: &Path,
) -> Result<DaemonEndpoints, DaemonError> {
  let endpoint_startup_lock = EndpointStartupLock::acquire(data_socket_path).map_err(|source| {
    DaemonError::EndpointStartupLock {
      path: endpoint_startup_lock_path(data_socket_path),
      source,
    }
  })?;
  let control_listener = bind_listener(control_socket_path).await?;
  let control_socket_guard = SocketGuard::new(control_socket_path.to_path_buf())?;
  // Publish the primary data endpoint last. `connect_or_start_daemon` waits
  // for this listener, so a successful data connection also proves that the
  // paired local-control endpoint is already bound.
  let listener = bind_listener(data_socket_path).await?;
  let data_socket_guard = SocketGuard::new(data_socket_path.to_path_buf())?;
  drop(endpoint_startup_lock);

  Ok(DaemonEndpoints {
    listener,
    control_listener,
    _data_socket_guard: data_socket_guard,
    _control_socket_guard: control_socket_guard,
  })
}

#[cfg(unix)]
async fn bind_listener(path: &Path) -> Result<Listener, DaemonError> {
  let listener = match Listener::bind(path) {
    Ok(listener) => listener,
    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
      if Stream::connect(path).await.is_ok() {
        return Err(DaemonError::AlreadyRunning(path.to_path_buf()));
      }
      std::fs::remove_file(path).map_err(|source| DaemonError::Bind {
        path: path.to_path_buf(),
        source,
      })?;
      Listener::bind(path).map_err(|source| DaemonError::Bind {
        path: path.to_path_buf(),
        source,
      })?
    }
    Err(source) => Err(DaemonError::Bind {
      path: path.to_path_buf(),
      source,
    })?,
  };
  secure_socket_endpoint(path).map_err(|source| DaemonError::Bind {
    path: path.to_path_buf(),
    source,
  })?;
  Ok(listener)
}

#[cfg(unix)]
fn secure_socket_endpoint(path: &Path) -> io::Result<()> {
  use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

  let metadata = std::fs::symlink_metadata(path)?;
  if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!("local endpoint {} is not a Unix socket", path.display()),
    ));
  }
  let expected_uid = rustix::process::getuid().as_raw();
  if metadata.uid() != expected_uid {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!("local endpoint {} is owned by another user", path.display()),
    ));
  }

  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
  let permissions = std::fs::metadata(path)?.permissions().mode();
  if permissions & 0o077 != 0 {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "local endpoint {} is accessible by other users",
        path.display()
      ),
    ));
  }
  Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartState {
  Accepting,
  Draining,
}

struct RestartCoordinator {
  state: Mutex<RestartState>,
  changed: Notify,
  /// Broadcast after a restart has been accepted and its acknowledgement has
  /// been attempted. Data connections are deliberately disposable: closing
  /// them bounds shutdown even when a client is asleep or is not reading.
  data_connection_shutdown: watch::Sender<bool>,
  /// Mirrors data cancellation for pre-existing local-control handlers. The
  /// accepting control handler sends its acknowledgement before this is set.
  control_connection_shutdown: watch::Sender<bool>,
}

impl RestartCoordinator {
  fn new() -> Self {
    // `send_replace` retains the value even before a handler subscribes, so a
    // data/control connection accepted after restart observes cancellation at
    // its initial preflight check.
    let (data_connection_shutdown, _data_connection_shutdown_receiver) = watch::channel(false);
    let (control_connection_shutdown, _control_connection_shutdown_receiver) =
      watch::channel(false);
    Self {
      state: Mutex::new(RestartState::Accepting),
      changed: Notify::new(),
      data_connection_shutdown,
      control_connection_shutdown,
    }
  }

  fn data_connection_shutdown_receiver(&self) -> watch::Receiver<bool> {
    self.data_connection_shutdown.subscribe()
  }

  fn control_connection_shutdown_receiver(&self) -> watch::Receiver<bool> {
    self.control_connection_shutdown.subscribe()
  }

  /// Runs a session-creation operation while the daemon is still accepting
  /// new sessions.
  ///
  /// The closure executes under the same gate used by
  /// [`Self::begin_cooperative_restart`]. This makes a create either complete
  /// before restart snapshots its session, or be rejected after quiescing
  /// begins; it cannot be created in between.
  fn run_while_accepting<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
    let state = lock_restart_state(&self.state);
    if *state == RestartState::Draining {
      return None;
    }
    Some(operation())
  }

  /// Atomically quiesces creates, snapshots all existing sessions, and asks
  /// each session to terminate.
  fn begin_cooperative_restart(
    &self,
    sessions: &SessionManager,
  ) -> Result<u32, RestartCoordinatorError> {
    let mut state = lock_restart_state(&self.state);
    if *state == RestartState::Draining {
      return Err(RestartCoordinatorError::AlreadyDraining);
    }

    let targets = sessions.snapshot_for_cooperative_restart();
    let target_count =
      u32::try_from(targets.len()).map_err(|_| RestartCoordinatorError::SessionCountOverflow)?;
    *state = RestartState::Draining;

    let termination_result = targets
      .iter()
      .try_for_each(|session| session.kill().map_err(RestartCoordinatorError::Termination));
    if let Err(error) = termination_result {
      // A failed PTY kill cannot be rolled back, but leaving the daemon
      // permanently quiesced would strand healthy remaining sessions. Return
      // to normal admission and report the partial destructive failure.
      *state = RestartState::Accepting;
      drop(state);
      self.changed.notify_waiters();
      return Err(error);
    }

    sessions.clear_managed();
    drop(state);
    self.changed.notify_waiters();
    Ok(target_count)
  }

  fn is_draining(&self) -> bool {
    *lock_restart_state(&self.state) == RestartState::Draining
  }

  /// Closes every current and subsequently accepted connection other than the
  /// control handler that just acknowledged this restart.
  ///
  /// This is intentionally separate from entering `Draining`: the accepting
  /// control connection must first be able to receive `RestartAccepted`.
  /// Existing attachment notifications are therefore best effort; socket
  /// closure is the guaranteed restart outcome.
  fn cancel_other_connections(&self) {
    let _previous_data = self.data_connection_shutdown.send_replace(true);
    let _previous_control = self.control_connection_shutdown.send_replace(true);
  }

  async fn changed(&self) {
    self.changed.notified().await;
  }
}

#[derive(Debug, thiserror::Error)]
enum RestartCoordinatorError {
  #[error("a cooperative restart is already in progress")]
  AlreadyDraining,
  #[error("too many sessions to report a cooperative restart result")]
  SessionCountOverflow,
  #[error("could not terminate a session during cooperative restart: {0}")]
  Termination(SessionControlError),
}

fn lock_restart_state(state: &Mutex<RestartState>) -> std::sync::MutexGuard<'_, RestartState> {
  match state.lock() {
    Ok(state) => state,
    Err(poisoned) => poisoned.into_inner(),
  }
}

async fn handle_local_control_connection(
  stream: Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
  mut control_connection_shutdown: watch::Receiver<bool>,
) -> Result<(), ConnectionError> {
  if *control_connection_shutdown.borrow() {
    return Ok(());
  }

  let operation = ctl_core::observability::Operation::diagnostic_at(
    ctl_core::observability::Event::ControlTransport,
    ctl_core::observability::Level::Trace,
    ctl_core::observability::Context::default(),
  );
  let result = tokio::select! {
    biased;
    _changed = control_connection_shutdown.changed() => Ok(()),
    result = handle_active_local_control_connection(stream, sessions, restart) => result,
  };
  operation.finish_at(
    if result.is_ok() {
      ctl_core::observability::Outcome::Succeeded
    } else {
      ctl_core::observability::Outcome::Failed
    },
    if result.is_ok() {
      ctl_core::observability::Level::Trace
    } else {
      ctl_core::observability::Level::Warn
    },
    result.as_ref().err().map(ConnectionError::diagnostic_code),
    None,
  );
  result
}

async fn accept_local_control_handshake(stream: &mut Stream) -> Result<bool, ConnectionError> {
  let Some(handshake) = read_local_control_request(stream, LOCAL_CONTROL_HANDSHAKE_TIMEOUT).await?
  else {
    return Ok(false);
  };
  let LocalControlClientMessage::Handshake { protocol } = handshake else {
    send_local_control_error(
      stream,
      LocalControlErrorCode::InvalidRequest,
      "the first local-control message must be a handshake",
    )
    .await?;
    return Ok(false);
  };
  let Some(protocol_version) = protocol.negotiate(LOCAL_CONTROL_SUPPORTED_PROTOCOL_VERSIONS) else {
    send_local_control_error(
      stream,
      LocalControlErrorCode::ProtocolVersionMismatch,
      &format!(
        "local-control client offered {:?}; this daemon supports {LOCAL_CONTROL_SUPPORTED_PROTOCOL_VERSIONS:?}",
        protocol.supported_versions,
      ),
    )
    .await?;
    return Ok(false);
  };

  write_local_control_frame(
    stream,
    &LocalControlServerMessage::HandshakeAccepted {
      protocol_version,
      protocols: vec![
        ctmux_proto::protocol_info(),
        ctmux_ipc::local_control_protocol_info(),
      ],
      restart_supported: true,
      build: Some(ctl_core::component::build_info()),
      data_protocol_version: Some(PROTOCOL_VERSION),
      managed_sessions_supported: true,
    },
  )
  .await?;

  Ok(true)
}

async fn handle_active_local_control_connection(
  mut stream: Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
) -> Result<(), ConnectionError> {
  if !accept_local_control_handshake(&mut stream).await? {
    return Ok(());
  }
  let Some(request) =
    read_local_control_request(&mut stream, LOCAL_CONTROL_ARMED_REQUEST_TIMEOUT).await?
  else {
    return Ok(());
  };
  match request {
    LocalControlClientMessage::ManageSession {
      expected_instance,
      task_id,
      run_id,
      operation,
    } => {
      let result = tokio::task::spawn_blocking(move || {
        restart.run_while_accepting(|| {
          sessions.manage(expected_instance.as_deref(), &task_id, &run_id, operation)
        })
      })
      .await?;
      match result {
        Some(Ok(response)) => write_local_control_frame(&mut stream, &response).await?,
        Some(Err(message)) => {
          send_local_control_error(&mut stream, LocalControlErrorCode::InvalidRequest, &message)
            .await?;
        }
        None => {
          send_local_control_error(
            &mut stream,
            LocalControlErrorCode::RestartInProgress,
            DAEMON_DRAINING_MESSAGE,
          )
          .await?;
        }
      }
    }
    LocalControlClientMessage::RestartDaemon => {
      match restart.begin_cooperative_restart(&sessions) {
        Ok(terminated_sessions) => {
          let acknowledgement = write_local_control_frame(
            &mut stream,
            &LocalControlServerMessage::RestartAccepted {
              terminated_sessions,
            },
          )
          .await;

          // `begin_cooperative_restart` has already made the destructive
          // transition. Even if the requester disconnects before observing
          // the acknowledgement, data handlers still need to be canceled so
          // an unread client cannot pin daemon shutdown. On the normal path
          // this happens strictly after `RestartAccepted` was sent.
          restart.cancel_other_connections();
          acknowledgement?;
        }
        Err(RestartCoordinatorError::AlreadyDraining) => {
          send_local_control_error(
            &mut stream,
            LocalControlErrorCode::RestartInProgress,
            "ctmuxd is already draining for a cooperative restart",
          )
          .await?;
        }
        Err(error) => {
          send_local_control_error(
            &mut stream,
            LocalControlErrorCode::Internal,
            &error.to_string(),
          )
          .await?;
        }
      }
    }
    LocalControlClientMessage::Handshake { .. } => {
      send_local_control_error(
        &mut stream,
        LocalControlErrorCode::InvalidRequest,
        "expected a local-control operation after handshake",
      )
      .await?;
    }
  }
  Ok(())
}

async fn read_local_control_request(
  stream: &mut Stream,
  request_timeout: Duration,
) -> Result<Option<LocalControlClientMessage>, ConnectionError> {
  timeout_at(
    Instant::now() + request_timeout,
    read_local_control_frame(stream),
  )
  .await
  .map_err(|_| ConnectionError::LocalControlTimeout)?
  .map_err(ConnectionError::LocalControl)
}

async fn send_local_control_error(
  stream: &mut Stream,
  code: LocalControlErrorCode,
  message: &str,
) -> Result<(), ConnectionError> {
  write_local_control_frame(
    stream,
    &LocalControlServerMessage::Error {
      code,
      message: message.into(),
    },
  )
  .await
  .map_err(ConnectionError::LocalControl)
}

async fn handle_connection(
  stream: Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
  mut data_connection_shutdown: watch::Receiver<bool>,
  attachment_liveness: AttachmentLiveness,
) -> Result<(), ConnectionError> {
  if *data_connection_shutdown.borrow() {
    return Ok(());
  }

  // A cooperative restart treats transport connections as disposable. Keep
  // the cancellation at the outermost level so it can interrupt not only
  // attachment liveness reads, but also a stalled raw handshake or a
  // backpressured response write.
  let operation = ctl_core::observability::Operation::diagnostic_at(
    ctl_core::observability::Event::SessionTransport,
    ctl_core::observability::Level::Trace,
    ctl_core::observability::Context::default(),
  );
  let result = tokio::select! {
    biased;
    _changed = data_connection_shutdown.changed() => Ok(()),
    result = handle_active_connection(stream, sessions, restart, attachment_liveness) => result,
  };
  operation.finish_at(
    if result.is_ok() {
      ctl_core::observability::Outcome::Succeeded
    } else {
      ctl_core::observability::Outcome::Failed
    },
    if result.is_ok() {
      ctl_core::observability::Level::Trace
    } else {
      ctl_core::observability::Level::Warn
    },
    result.as_ref().err().map(ConnectionError::diagnostic_code),
    None,
  );
  result
}

async fn handle_active_connection(
  mut stream: Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
  attachment_liveness: AttachmentLiveness,
) -> Result<(), ConnectionError> {
  let Some(handshake) = read_frame::<_, ClientMessage>(&mut stream).await? else {
    return Ok(());
  };

  let ClientMessage::Handshake { protocol, .. } = handshake else {
    send_error(
      &mut stream,
      ErrorCode::InvalidRequest,
      "the first message must be a handshake",
    )
    .await?;
    return Ok(());
  };

  let Some(protocol_version) = protocol.negotiate(SUPPORTED_PROTOCOL_VERSIONS) else {
    send_error(
      &mut stream,
      ErrorCode::ProtocolVersionMismatch,
      &format!(
        "client offered protocol contracts {:?}; this daemon supports {SUPPORTED_PROTOCOL_VERSIONS:?}",
        protocol.supported_versions,
      ),
    )
    .await?;
    return Ok(());
  };

  write_frame(
    &mut stream,
    &ServerMessage::HandshakeAccepted {
      protocol_version,
      protocols: vec![
        ctmux_proto::protocol_info(),
        ctmux_ipc::local_control_protocol_info(),
      ],
      server_version: SERVER_VERSION.into(),
      build: Some(ctl_core::component::build_info()),
      heartbeat_interval_ms: attachment_liveness.heartbeat_interval_ms,
      attachment_liveness_timeout_ms: attachment_liveness.timeout_ms,
    },
  )
  .await?;

  handle_request(
    stream,
    sessions,
    restart,
    attachment_liveness.timeout,
    protocol_version,
  )
  .await
}

async fn handle_request(
  mut stream: Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
  attachment_liveness_timeout: Duration,
  protocol_version: ctl_core::protocol::ProtocolVersion,
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
    } => {
      handle_create_session_request(
        &mut stream,
        sessions,
        restart,
        CreateSessionParameters {
          name,
          command,
          working_directory,
          terminal_size,
        },
      )
      .await?;
    }
    ClientMessage::ListSessions => {
      let response = ServerMessage::SessionList {
        sessions: sessions.list(),
      };
      write_frame(&mut stream, &response).await?;
    }
    ClientMessage::GetShellState { session } => {
      handle_shell_state_request(&mut stream, &sessions, session).await?;
    }
    ClientMessage::AttachSession {
      session,
      resume_from,
      terminal_size,
      request_input_lease,
      request_layout_lease,
      request_command_line,
      request_running_command,
      presentation_window_bytes,
    } => {
      let request = AttachParameters {
        protocol_version,
        resume_from,
        client_terminal_size: terminal_size,
        request_input_lease,
        request_layout_lease,
        request_command_line,
        request_running_command,
        presentation_window_bytes,
        attachment_liveness_timeout,
      };
      return handle_new_attachment_request(stream, sessions, restart, session, request).await;
    }
    ClientMessage::ResumeAttachment {
      session,
      attachment_token,
      resume_from,
      terminal_size,
      request_command_line,
      request_running_command,
      presentation_window_bytes,
    } => {
      let request = AttachParameters {
        protocol_version,
        resume_from,
        client_terminal_size: terminal_size,
        request_input_lease: false,
        request_layout_lease: false,
        request_command_line,
        request_running_command,
        presentation_window_bytes,
        attachment_liveness_timeout,
      };
      return handle_resume_attachment_request(
        stream,
        sessions,
        restart,
        session,
        attachment_token,
        request,
      )
      .await;
    }
    ClientMessage::KillSession { session } => {
      handle_kill_session_request(&mut stream, &sessions, &session).await?;
    }
    request => {
      return handle_view_request(stream, sessions, restart, request, protocol_version).await;
    }
  }

  Ok(())
}

async fn handle_view_request(
  mut stream: Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
  request: ClientMessage,
  protocol_version: ctl_core::protocol::ProtocolVersion,
) -> Result<(), ConnectionError> {
  match request {
    ClientMessage::GetView { session } => {
      write_view_result(&mut stream, sessions.view(&session), protocol_version).await?;
    }
    ClientMessage::UpdateView {
      session,
      expected_revision,
      layout,
    } => {
      if !ctmux_proto::supports_pane_resize(protocol_version) && layout.has_weights() {
        send_error(
          &mut stream,
          ErrorCode::InvalidRequest,
          "split weights require contract 1.1.16",
        )
        .await?;
        return Ok(());
      }
      let result = tokio::task::spawn_blocking(move || {
        sessions.update_view(&session, expected_revision, layout)
      })
      .await?;
      write_view_result(&mut stream, result, protocol_version).await?;
    }
    ClientMessage::PromoteTerminal { terminal_id, name } => {
      let result =
        tokio::task::spawn_blocking(move || sessions.promote_terminal(&terminal_id, name)).await?;
      write_view_result(&mut stream, result, protocol_version).await?;
    }
    ClientMessage::MergeSessions {
      source,
      destination,
    } => {
      let result =
        tokio::task::spawn_blocking(move || sessions.merge_sessions(&source, &destination)).await?;
      write_view_result(&mut stream, result, protocol_version).await?;
    }
    ClientMessage::SplitTerminal {
      terminal_id,
      axis,
      command,
      working_directory,
      terminal_size,
    } => {
      let result = tokio::task::spawn_blocking(move || {
        restart
          .run_while_accepting(|| {
            let terminal = sessions.split_terminal(
              terminal_id,
              axis,
              command,
              working_directory,
              terminal_size,
            )?;
            sessions.view(&terminal.info().session_id)
          })
          .unwrap_or_else(|| {
            Err(SessionManagerError::InvalidView(
              DAEMON_DRAINING_MESSAGE.into(),
            ))
          })
      })
      .await?;
      write_view_result(&mut stream, result, protocol_version).await?;
    }
    ClientMessage::KillTerminal { terminal_id } => match sessions.resolve(&terminal_id) {
      Ok(terminal) if terminal.info().terminal_id == terminal_id => match terminal.kill() {
        Ok(()) => write_frame(&mut stream, &ServerMessage::Success).await?,
        Err(error) => send_control_error(&mut stream, &error).await?,
      },
      Ok(_) => {
        send_error(
          &mut stream,
          ErrorCode::InvalidRequest,
          "expected a terminal ID",
        )
        .await?;
      }
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

async fn write_view_result(
  stream: &mut Stream,
  result: Result<ctmux_proto::ViewInfo, SessionManagerError>,
  protocol_version: ctl_core::protocol::ProtocolVersion,
) -> Result<(), CodecError> {
  match result {
    Ok(view) => {
      let view = view_for_contract(view, protocol_version);
      write_frame(stream, &ServerMessage::ViewSnapshot { view }).await
    }
    Err(error) => send_session_manager_error(stream, &error).await,
  }
}

fn view_for_contract(
  mut view: ctmux_proto::ViewInfo,
  protocol_version: ctl_core::protocol::ProtocolVersion,
) -> ctmux_proto::ViewInfo {
  if !ctmux_proto::supports_view_zoom(protocol_version) {
    view.zoomed_terminal_id = None;
  }
  if !ctmux_proto::supports_pane_resize(protocol_version) {
    view.layout.clear_weights();
  }
  view
}

async fn handle_kill_session_request(
  stream: &mut Stream,
  sessions: &SessionManager,
  session: &str,
) -> Result<(), ConnectionError> {
  match sessions.begin_termination(session) {
    Ok(terminals) => {
      for terminal in terminals {
        if let Err(error) = terminal.kill() {
          send_control_error(stream, &error).await?;
          return Ok(());
        }
      }
      write_frame(stream, &ServerMessage::Success).await?;
    }
    Err(error) => send_session_manager_error(stream, &error).await?,
  }
  Ok(())
}

async fn handle_new_attachment_request(
  mut stream: Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
  session: String,
  request: AttachParameters,
) -> Result<(), ConnectionError> {
  if reject_invalid_presentation_window(&mut stream, &request).await? {
    return Ok(());
  }
  match restart.run_while_accepting(|| {
    sessions
      .resolve(&session)
      .map(|session| prepare_attachment(session, &request))
  }) {
    Some(Ok(Ok(attachment))) => handle_attach(stream, attachment, request).await,
    Some(Ok(Err(exit_code))) => {
      send_error(
        &mut stream,
        ErrorCode::SessionNotFound,
        &format!("session '{session}' has already ended (exit code {exit_code:?})"),
      )
      .await?;
      Ok(())
    }
    Some(Err(error)) => {
      send_session_manager_error(&mut stream, &error).await?;
      Ok(())
    }
    None => {
      send_error(
        &mut stream,
        ErrorCode::InvalidRequest,
        DAEMON_DRAINING_MESSAGE,
      )
      .await?;
      Ok(())
    }
  }
}

async fn handle_resume_attachment_request(
  mut stream: Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
  session: String,
  attachment_token: String,
  request: AttachParameters,
) -> Result<(), ConnectionError> {
  if reject_invalid_presentation_window(&mut stream, &request).await? {
    return Ok(());
  }
  match restart.run_while_accepting(|| {
    sessions
      .resolve(&session)
      .map(|session| prepare_resumed_attachment(session, &attachment_token))
  }) {
    Some(Ok(Ok(attachment))) => handle_attach(stream, attachment, request).await,
    Some(Ok(Err(ResumePreparationError::SessionEnded(exit_code)))) => {
      send_error(
        &mut stream,
        ErrorCode::SessionNotFound,
        &format!("session '{session}' has already ended (exit code {exit_code:?})"),
      )
      .await?;
      Ok(())
    }
    Some(Ok(Err(ResumePreparationError::Rejected))) => {
      send_error(
        &mut stream,
        ErrorCode::AttachmentResumeRejected,
        "the attachment reconnect token is invalid or expired",
      )
      .await?;
      Ok(())
    }
    Some(Err(error)) => {
      send_session_manager_error(&mut stream, &error).await?;
      Ok(())
    }
    None => {
      send_error(
        &mut stream,
        ErrorCode::InvalidRequest,
        DAEMON_DRAINING_MESSAGE,
      )
      .await?;
      Ok(())
    }
  }
}

struct CreateSessionParameters {
  name: Option<String>,
  command: Option<ctmux_proto::CommandSpec>,
  working_directory: Option<String>,
  terminal_size: ctmux_proto::TerminalSize,
}

async fn handle_create_session_request(
  stream: &mut Stream,
  sessions: SessionManager,
  restart: Arc<RestartCoordinator>,
  request: CreateSessionParameters,
) -> Result<(), ConnectionError> {
  let CreateSessionParameters {
    name,
    command,
    working_directory,
    terminal_size,
  } = request;
  match tokio::task::spawn_blocking(move || {
    restart.run_while_accepting(|| sessions.create(name, command, working_directory, terminal_size))
  })
  .await?
  {
    Some(Ok(session)) => {
      write_frame(
        stream,
        &ServerMessage::SessionCreated {
          session: session.info(),
        },
      )
      .await?;
    }
    Some(Err(error)) => send_session_manager_error(stream, &error).await?,
    None => {
      send_error(stream, ErrorCode::InvalidRequest, DAEMON_DRAINING_MESSAGE).await?;
    }
  }
  Ok(())
}

async fn handle_shell_state_request(
  stream: &mut Stream,
  sessions: &SessionManager,
  session: String,
) -> Result<(), ConnectionError> {
  match sessions.resolve(&session) {
    Ok(session) => {
      write_frame(
        stream,
        &ServerMessage::ShellStateResponse {
          session: session.info(),
          shell_state: session.shell_state_for_inspection(),
        },
      )
      .await?;
    }
    Err(error) => send_session_manager_error(stream, &error).await?,
  }
  Ok(())
}

#[allow(
  clippy::struct_excessive_bools,
  reason = "each field is an independent attachment behavior negotiated by the protocol"
)]
struct AttachParameters {
  protocol_version: ctl_core::protocol::ProtocolVersion,
  resume_from: Option<u64>,
  client_terminal_size: ctmux_proto::TerminalSize,
  request_input_lease: bool,
  request_layout_lease: bool,
  request_command_line: bool,
  request_running_command: bool,
  presentation_window_bytes: u64,
  attachment_liveness_timeout: Duration,
}

fn valid_presentation_window(window_bytes: u64) -> bool {
  (1..=MAX_PRESENTATION_WINDOW_BYTES).contains(&window_bytes)
}

async fn reject_invalid_presentation_window(
  stream: &mut Stream,
  request: &AttachParameters,
) -> Result<bool, ConnectionError> {
  if valid_presentation_window(request.presentation_window_bytes) {
    return Ok(false);
  }
  send_error(
    stream,
    ErrorCode::InvalidRequest,
    "presentation_window_bytes must be between 1 and 8388608",
  )
  .await?;
  Ok(true)
}

/// Synchronous attachment admission established under the daemon restart gate.
///
/// `events` is subscribed before the gate is released, so a subsequent
/// cooperative restart cannot publish `SessionEnded` before this attachment is
/// ready to receive it.
struct PreparedAttachment {
  session: Arc<Terminal>,
  attachment_id: String,
  attachment_token: String,
  attachment_generation: u64,
  attachment_leases: ctmux_core::AttachmentLeases,
  superseded: watch::Receiver<u64>,
  resumed: bool,
  events: broadcast::Receiver<SessionEvent>,
  shell_state_updates: watch::Receiver<ShellState>,
  view_updates: watch::Receiver<Option<ctmux_proto::ViewInfo>>,
  layout_lease_updates: watch::Receiver<()>,
}

impl PreparedAttachment {
  fn guard(&self, reconnect_grace: Duration) -> AttachmentGuard {
    AttachmentGuard {
      session: Arc::clone(&self.session),
      attachment_token: self.attachment_token.clone(),
      generation: self.attachment_generation,
      reconnect_grace,
      preserve_on_drop: self.resumed,
      active: true,
    }
  }
}

fn prepare_attachment(
  session: Arc<Terminal>,
  request: &AttachParameters,
) -> Result<PreparedAttachment, Option<u32>> {
  // Subscribe before acquiring leases. If the child exits at any later point,
  // the receiver already holds its terminal event; if it exited beforehand,
  // `subscribe_events` reports that fact instead of returning a permanently
  // silent receiver.
  let events = session.subscribe_events()?;
  let registration =
    session.create_attachment(request.request_input_lease, request.request_layout_lease);
  let shell_state_updates = session.subscribe_shell_state();
  Ok(prepared_attachment(
    session,
    registration,
    false,
    events,
    shell_state_updates,
  ))
}

enum ResumePreparationError {
  SessionEnded(Option<u32>),
  Rejected,
}

fn prepare_resumed_attachment(
  session: Arc<Terminal>,
  attachment_token: &str,
) -> Result<PreparedAttachment, ResumePreparationError> {
  let events = session
    .subscribe_events()
    .map_err(ResumePreparationError::SessionEnded)?;
  let registration = session
    .resume_attachment(attachment_token)
    .ok_or(ResumePreparationError::Rejected)?;
  let shell_state_updates = session.subscribe_shell_state();
  Ok(prepared_attachment(
    session,
    registration,
    true,
    events,
    shell_state_updates,
  ))
}

fn prepared_attachment(
  session: Arc<Terminal>,
  registration: AttachmentRegistration,
  resumed: bool,
  events: broadcast::Receiver<SessionEvent>,
  shell_state_updates: watch::Receiver<ShellState>,
) -> PreparedAttachment {
  let mut view_updates = session.subscribe_view();
  let mut layout_lease_updates = session.subscribe_layout_leases();
  // Deliver the current view even when this attachment does not resize it.
  // Subscription precedes initial PTY work, so later mutations cannot be lost.
  view_updates.mark_changed();
  // Admission can race a lease change before the initial attached reply.
  layout_lease_updates.mark_changed();
  PreparedAttachment {
    session,
    attachment_id: registration.attachment_id,
    attachment_token: registration.attachment_token,
    attachment_generation: registration.generation,
    attachment_leases: registration.leases,
    superseded: registration.superseded,
    resumed,
    events,
    shell_state_updates,
    view_updates,
    layout_lease_updates,
  }
}

async fn handle_attach(
  mut stream: Stream,
  attachment: PreparedAttachment,
  request: AttachParameters,
) -> Result<(), ConnectionError> {
  let mut attachment_guard = attachment.guard(request.attachment_liveness_timeout);
  let initial_delivery_deadline = initial_attachment_delivery_deadline();

  if attachment.attachment_leases.layout.owned_by_client
    && !apply_initial_resize(
      &mut stream,
      Arc::clone(&attachment.session),
      attachment.attachment_id.clone(),
      request.client_terminal_size.clone(),
      initial_delivery_deadline,
    )
    .await?
  {
    return Ok(());
  }

  let Some(mut snapshot) = take_initial_snapshot(
    &mut stream,
    Arc::clone(&attachment.session),
    request.resume_from,
    initial_delivery_deadline,
  )
  .await?
  else {
    return Ok(());
  };
  let initial_shell_state = shell_state_for_attachment(
    snapshot.shell_state.clone(),
    request.request_command_line && attachment.attachment_leases.input.owned_by_client,
    request.request_running_command && attachment.attachment_leases.input.owned_by_client,
  );
  let checkpoint_geometry_revision = snapshot.checkpoint_geometry_revision;
  let sent_sequence = snapshot.journal.replay_from;
  let applied_sequence = snapshot.checkpoint.is_none().then_some(sent_sequence);
  let pinned_history = pin_snapshot_history(&mut snapshot, request.protocol_version)?;
  let (reader, mut writer) = tokio::io::split(stream);
  if !send_initial_attachment(
    &mut writer,
    snapshot,
    &request,
    &attachment,
    initial_shell_state.clone(),
    pinned_history
      .as_ref()
      .map(|history| history.manifest.clone()),
    initial_delivery_deadline,
  )
  .await?
  {
    return Ok(());
  }
  attachment_guard.preserve_on_drop = true;

  let live_attachment = LiveAttachment {
    reader: FrameReader::new(reader),
    writer,
    events: attachment.events,
    shell_state_updates: attachment.shell_state_updates,
    view_updates: attachment.view_updates,
    layout_lease_updates: attachment.layout_lease_updates,
    layout_lease_status: attachment.attachment_leases.layout,
    sent_sequence,
    checkpoint_geometry_revision,
    shell_state_revision: initial_shell_state.revision,
    presentation_window_bytes: request.presentation_window_bytes,
    applied_sequence,
    in_flight_output: VecDeque::new(),
    in_flight_charge_bytes: 0,
    request_command_line: request.request_command_line,
    request_running_command: request.request_running_command,
    superseded: attachment.superseded,
    pinned_history,
    pending_history: None,
    pending_checkpoint: false,
    protocol_version: request.protocol_version,
  };
  let exit = drive_attachment(
    live_attachment,
    attachment.session,
    attachment.attachment_id,
    request.attachment_liveness_timeout,
    Instant::now() + request.attachment_liveness_timeout,
  )
  .await?;
  if exit == AttachmentExit::Detached {
    attachment_guard.close_now();
  }
  Ok(())
}

async fn send_initial_attachment(
  writer: &mut OwnedWriteHalf,
  snapshot: AttachSnapshot,
  request: &AttachParameters,
  attachment: &PreparedAttachment,
  shell_state: ShellState,
  history_manifest: Option<TerminalHistoryManifest>,
  deadline: Instant,
) -> Result<bool, ConnectionError> {
  match timeout_at(
    deadline,
    send_attached(
      writer,
      snapshot,
      request.client_terminal_size.clone(),
      attachment.attachment_leases.clone(),
      attachment.attachment_token.clone(),
      shell_state,
      history_manifest,
    ),
  )
  .await
  {
    Ok(result) => result.map(|()| true),
    Err(_) => Ok(false),
  }
}

async fn apply_initial_resize(
  stream: &mut Stream,
  session: Arc<Terminal>,
  attachment_id: String,
  terminal_size: ctmux_proto::TerminalSize,
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
  stream: &mut Stream,
  session: Arc<Terminal>,
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
  protocol_version: ctl_core::protocol::ProtocolVersion,
  reader: FrameReader<OwnedReadHalf>,
  writer: OwnedWriteHalf,
  events: broadcast::Receiver<SessionEvent>,
  shell_state_updates: watch::Receiver<ShellState>,
  view_updates: watch::Receiver<Option<ctmux_proto::ViewInfo>>,
  layout_lease_updates: watch::Receiver<()>,
  layout_lease_status: ctmux_proto::LeaseStatus,
  sent_sequence: u64,
  /// Internal ordering for geometry changes represented by the last checkpoint.
  checkpoint_geometry_revision: Option<u64>,
  shell_state_revision: u64,
  presentation_window_bytes: u64,
  applied_sequence: Option<u64>,
  in_flight_output: VecDeque<OutputDelivery>,
  in_flight_charge_bytes: u64,
  request_command_line: bool,
  request_running_command: bool,
  superseded: watch::Receiver<u64>,
  pinned_history: Option<PinnedHistory>,
  pending_history: Option<HistoryRequest>,
  pending_checkpoint: bool,
}

struct HistoryRequest {
  snapshot_id: String,
  offset: u64,
  max_bytes: u64,
}

/// One bounded immutable transfer snapshot, not a persistent history log.
struct PinnedHistory {
  manifest: TerminalHistoryManifest,
  data: Arc<Vec<u8>>,
  last_access: Instant,
  served_end: usize,
}

impl PinnedHistory {
  fn new(
    history: &TerminalHistorySnapshot,
    rows: &PhysicalHistory,
    scrollback_limit: u64,
  ) -> Result<(Self, TerminalHistorySnapshot), ConnectionError> {
    let encoded = rows.encoded()?;
    let data = Arc::clone(&encoded.data);
    let lines = rows.lines();
    let mut first_line = lines.len();
    let mut recent_bytes = 0;
    while first_line > 0 && lines.len() - first_line < MAX_RECENT_HISTORY_LINES {
      let bytes = serde_json::to_vec(&lines[first_line - 1])
        .map_err(io::Error::other)?
        .len()
        + 1;
      if recent_bytes + bytes > MAX_RECENT_HISTORY_BYTES {
        break;
      }
      recent_bytes += bytes;
      first_line -= 1;
    }
    let manifest = TerminalHistoryManifest {
      snapshot_id: uuid::Uuid::new_v4().to_string(),
      sequence: history.sequence,
      generation: history.generation,
      revision: history.revision,
      total_rows: rows.len() as u64,
      total_bytes: data.len() as u64,
      total_lines: lines.len() as u64,
      first_line: first_line as u64,
      truncated: history.truncated,
      content_hash: encoded.content_hash.clone(),
      scrollback_limit,
    };
    let recent = TerminalHistorySnapshot {
      format: history.format.clone(),
      format_version: history.format_version,
      sequence: history.sequence,
      generation: history.generation,
      revision: history.revision,
      retained_bytes: history.retained_bytes,
      truncated: history.truncated,
      lines: lines[first_line..].to_vec(),
    };
    Ok((
      Self {
        manifest,
        data,
        last_access: Instant::now(),
        served_end: 0,
      },
      recent,
    ))
  }

  fn expired(&self) -> bool {
    self.last_access.elapsed() >= HISTORY_SNAPSHOT_IDLE_TTL
  }

  fn complete(&self) -> bool {
    self.served_end == self.data.len()
  }

  fn page(&mut self, request: &HistoryRequest) -> Result<ServerMessage, ErrorCode> {
    if self.expired() || request.snapshot_id != self.manifest.snapshot_id {
      return Ok(ServerMessage::HistorySnapshotExpired {
        snapshot_id: request.snapshot_id.clone(),
      });
    }
    let offset = usize::try_from(request.offset).map_err(|_| ErrorCode::InvalidRequest)?;
    if offset > self.data.len() || request.max_bytes == 0 {
      return Err(ErrorCode::InvalidRequest);
    }
    let max_bytes = usize::try_from(request.max_bytes)
      .unwrap_or(usize::MAX)
      .min(MAX_HISTORY_PAGE_BYTES);
    let end = offset.saturating_add(max_bytes).min(self.data.len());
    if offset <= self.served_end {
      self.served_end = self.served_end.max(end);
    }
    self.last_access = Instant::now();
    Ok(ServerMessage::HistoryPage {
      snapshot_id: self.manifest.snapshot_id.clone(),
      offset: request.offset,
      data: self.data[offset..end].to_vec(),
      next_offset: (end < self.data.len()).then_some(end as u64),
    })
  }
}

fn pin_snapshot_history(
  snapshot: &mut AttachSnapshot,
  protocol_version: ctl_core::protocol::ProtocolVersion,
) -> Result<Option<PinnedHistory>, ConnectionError> {
  project_checkpoint_for_contract(snapshot, protocol_version);
  if protocol_version == ctmux_proto::CONTRACT_V1_0_13 {
    // Preserve the complete inline history for the first published contract.
    return Ok(None);
  }
  let Some(history) = snapshot.history.as_ref() else {
    return Ok(None);
  };
  let empty = PhysicalHistory::default();
  let rows = snapshot.history_rows.as_deref().unwrap_or(&empty);
  let (pinned, recent) = PinnedHistory::new(history, rows, snapshot.scrollback_limit)?;
  snapshot.history = Some(recent);
  Ok(Some(pinned))
}

fn project_checkpoint_for_contract(
  snapshot: &mut AttachSnapshot,
  protocol_version: ctl_core::protocol::ProtocolVersion,
) {
  if ctmux_proto::supports_extended_keys(protocol_version) {
    return;
  }
  // Remove only the captured keyboard preamble. Identical bytes in application
  // strings and the emulator's incomplete parser state must remain untouched.
  let prefix_bytes = std::mem::take(&mut snapshot.checkpoint_keyboard_prefix_bytes);
  if let Some(checkpoint) = snapshot.checkpoint.as_mut() {
    checkpoint.payload.drain(..prefix_bytes);
  }
}

#[derive(Debug, Clone, Copy)]
struct OutputDelivery {
  sequence_end: u64,
  charge_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentExit {
  Detached,
  Disconnected,
  Superseded,
}

async fn drive_attachment(
  attachment: LiveAttachment,
  session: Arc<Terminal>,
  attachment_id: String,
  attachment_liveness_timeout: Duration,
  deadline: Instant,
) -> Result<AttachmentExit, ConnectionError> {
  let mut driver = AttachmentDriver {
    attachment,
    session,
    attachment_id,
    attachment_liveness_timeout,
    deadline,
    pending_session_end: None,
  };
  if !driver.send_available_output().await? {
    return Ok(AttachmentExit::Disconnected);
  }
  loop {
    if driver
      .attachment
      .pinned_history
      .as_ref()
      .is_some_and(PinnedHistory::expired)
    {
      driver.attachment.pinned_history = None;
    }
    // Give the terminal event queue bounded progress even when incoming
    // heartbeats are continuously ready. Process only one queued event here
    // so presentation acknowledgements still get a turn during heavy output.
    let queued_event = match driver.attachment.events.try_recv() {
      Ok(event) => Some(Ok(event)),
      Err(broadcast::error::TryRecvError::Closed) => Some(Err(broadcast::error::RecvError::Closed)),
      Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
        Some(Err(broadcast::error::RecvError::Lagged(skipped)))
      }
      Err(broadcast::error::TryRecvError::Empty) => None,
    };
    if let Some(event) = queued_event
      && !driver.process_session_event(event).await?
    {
      return Ok(AttachmentExit::Disconnected);
    }
    // Like terminal events, shared view metadata gets one bounded turn even
    // while presentation acknowledgements or history requests stay ready.
    if !driver.send_pending_metadata().await? {
      return Ok(AttachmentExit::Disconnected);
    }
    if driver.send_session_end_if_drained().await? {
      return Ok(AttachmentExit::Disconnected);
    }
    tokio::select! {
      biased;
      () = sleep_until(driver.deadline) => return Ok(AttachmentExit::Disconnected),
      changed = driver.attachment.superseded.changed() => {
        let _ = changed;
        return Ok(AttachmentExit::Superseded);
      }
      incoming = driver.attachment.reader.read_frame::<ClientMessage>() => {
        let Some(message) = incoming? else {
          return Ok(AttachmentExit::Disconnected);
        };
        if message == ClientMessage::Detach {
          // Receipt of `detach` is authoritative even when its acknowledgement
          // cannot cross a concurrently failing transport. The guard still
          // closes the logical attachment immediately in either case.
          let _acknowledgement = write_before_deadline(
            &mut driver.attachment.writer,
            &ServerMessage::Detached,
            driver.deadline,
          )
          .await;
          return Ok(AttachmentExit::Detached);
        }
        if !driver.process_client_message(message).await? {
          return Ok(AttachmentExit::Disconnected);
        }
      }
      // Output gets a bounded turn above; control always precedes this small
      // page. A continuous PTY producer must not starve history indefinitely.
      () = std::future::ready(()), if driver.attachment.pending_history.is_some() => {
        if !driver.send_available_output().await? || !driver.send_history_page().await? {
          return Ok(AttachmentExit::Disconnected);
        }
      }
      event = driver.attachment.events.recv() => {
        if !driver.process_session_event(event).await? {
          return Ok(AttachmentExit::Disconnected);
        }
      }
      // Raw PTY output is canonical and must take precedence over advisory
      // metadata. A watch receiver coalesces state updates, so delaying one
      // here never loses the latest snapshot. During final output draining,
      // the final snapshot is delivered immediately before SessionEnded.
      changed = driver.attachment.shell_state_updates.changed(), if driver.pending_session_end.is_none() => {
        if !driver.process_shell_state_update(changed).await? {
          return Ok(AttachmentExit::Disconnected);
        }
      }
      changed = driver.attachment.view_updates.changed(), if ctmux_proto::supports_view_zoom(driver.attachment.protocol_version) && driver.pending_session_end.is_none() => {
        if changed.is_err() || !driver.send_view_update().await? {
          return Ok(AttachmentExit::Disconnected);
        }
      }
      changed = driver.attachment.layout_lease_updates.changed(), if ctmux_proto::supports_layout_lease_notifications(driver.attachment.protocol_version) && driver.pending_session_end.is_none() => {
        if changed.is_err() || !driver.send_layout_lease_update().await? {
          return Ok(AttachmentExit::Disconnected);
        }
      }
    }
  }
}

struct AttachmentDriver {
  attachment: LiveAttachment,
  session: Arc<Terminal>,
  attachment_id: String,
  attachment_liveness_timeout: Duration,
  deadline: Instant,
  pending_session_end: Option<PendingSessionEnd>,
}

#[derive(Clone, Copy)]
struct PendingSessionEnd {
  exit_code: Option<u32>,
  final_sequence: u64,
}

impl AttachmentDriver {
  async fn send_pending_metadata(&mut self) -> Result<bool, ConnectionError> {
    if self.pending_session_end.is_some() {
      return Ok(true);
    }
    if ctmux_proto::supports_view_zoom(self.attachment.protocol_version)
      && self.attachment.view_updates.has_changed().unwrap_or(false)
      && !self.send_view_update().await?
    {
      return Ok(false);
    }
    if ctmux_proto::supports_layout_lease_notifications(self.attachment.protocol_version)
      && self
        .attachment
        .layout_lease_updates
        .has_changed()
        .unwrap_or(false)
      && !self.send_layout_lease_update().await?
    {
      return Ok(false);
    }
    Ok(true)
  }

  async fn send_layout_lease_update(&mut self) -> Result<bool, ConnectionError> {
    self.attachment.layout_lease_updates.borrow_and_update();
    let status = self.session.layout_lease_status(&self.attachment_id);
    if status == self.attachment.layout_lease_status {
      return Ok(true);
    }
    let written = write_before_deadline(
      &mut self.attachment.writer,
      &ServerMessage::LeaseStatus {
        lease: LeaseKind::Layout,
        status: status.clone(),
        notification: true,
      },
      self.deadline,
    )
    .await?;
    if written.is_some() {
      self.attachment.layout_lease_status = status;
    }
    Ok(written.is_some())
  }

  async fn process_lease_change(
    &mut self,
    lease: LeaseKind,
    acquire: bool,
  ) -> Result<bool, ConnectionError> {
    let previously_owned_input =
      lease == LeaseKind::Input && self.session.owns_input_lease(&self.attachment_id);
    let status = if acquire {
      self.session.acquire_lease(&self.attachment_id, lease)
    } else {
      self.session.release_lease(&self.attachment_id, lease)
    };
    let owns_input = status.owned_by_client;
    if lease == LeaseKind::Layout {
      // The requester receives this direct reply. Its watch notification must
      // not look like a second acknowledgement to clients with queued intents.
      self.attachment.layout_lease_status = status.clone();
    }
    let written = write_before_deadline(
      &mut self.attachment.writer,
      &ServerMessage::LeaseStatus {
        lease,
        status,
        notification: false,
      },
      self.deadline,
    )
    .await?;
    if lease == LeaseKind::Input
      && (self.attachment.request_command_line || self.attachment.request_running_command)
      && previously_owned_input != owns_input
    {
      self.session.refresh_shell_state_for_visibility();
    }
    Ok(written.is_some())
  }

  async fn send_view_update(&mut self) -> Result<bool, ConnectionError> {
    let view = self.attachment.view_updates.borrow_and_update().clone();
    let Some(view) = view else {
      return Ok(true);
    };
    let view = view_for_contract(view, self.attachment.protocol_version);
    write_before_deadline(
      &mut self.attachment.writer,
      &ServerMessage::ViewSnapshot { view },
      self.deadline,
    )
    .await
    .map(|written| written.is_some())
  }

  async fn process_client_message(
    &mut self,
    message: ClientMessage,
  ) -> Result<bool, ConnectionError> {
    if Instant::now() >= self.deadline {
      return Ok(false);
    }
    if let ClientMessage::PresentationApplied { sequence } = message {
      if let Err(message) = self.accept_presentation_progress(sequence) {
        return write_before_deadline(
          &mut self.attachment.writer,
          &ServerMessage::Error {
            code: ErrorCode::InvalidRequest,
            message: message.into(),
          },
          self.deadline,
        )
        .await
        .map(|written| written.is_some());
      }
      self.renew_liveness();
      return self.send_available_output().await;
    }
    match message {
      ClientMessage::AcquireLease { lease } => {
        self.renew_liveness();
        return self.process_lease_change(lease, true).await;
      }
      ClientMessage::ReleaseLease { lease } => {
        self.renew_liveness();
        return self.process_lease_change(lease, false).await;
      }
      ClientMessage::RequestCheckpoint | ClientMessage::HistoryRequest { .. }
        if self.attachment.protocol_version == ctmux_proto::CONTRACT_V1_0_13 =>
      {
        return write_before_deadline(
          &mut self.attachment.writer,
          &ServerMessage::Error {
            code: ErrorCode::InvalidRequest,
            message: "paged history requires contract 1.1.14".into(),
          },
          self.deadline,
        )
        .await
        .map(|written| written.is_some());
      }
      ClientMessage::RequestCheckpoint => {
        self.attachment.pending_checkpoint = true;
        self.renew_liveness();
        return self.send_available_output().await;
      }
      ClientMessage::HistoryRequest {
        snapshot_id,
        offset,
        max_bytes,
      } => {
        self.renew_liveness();
        if self.attachment.pending_history.is_some() {
          return write_before_deadline(
            &mut self.attachment.writer,
            &ServerMessage::Error {
              code: ErrorCode::InvalidRequest,
              message: "only one history page request may be pending".into(),
            },
            self.deadline,
          )
          .await
          .map(|written| written.is_some());
        }
        self.attachment.pending_history = Some(HistoryRequest {
          snapshot_id,
          offset,
          max_bytes,
        });
        return Ok(true);
      }
      _ => {}
    }
    if renews_attachment_liveness(&message) {
      self.renew_liveness();
    }
    match timeout_at(
      self.deadline,
      process_attach_input(
        &mut self.attachment.writer,
        Arc::clone(&self.session),
        &self.attachment_id,
        self.attachment.protocol_version,
        message,
      ),
    )
    .await
    {
      Ok(result) => result,
      Err(_) => Ok(false),
    }
  }

  async fn send_history_page(&mut self) -> Result<bool, ConnectionError> {
    let Some(request) = self.attachment.pending_history.take() else {
      return Ok(true);
    };
    let response = self.attachment.pinned_history.as_mut().map_or_else(
      || {
        Ok(ServerMessage::HistorySnapshotExpired {
          snapshot_id: request.snapshot_id.clone(),
        })
      },
      |history| history.page(&request),
    );
    let message = response.unwrap_or_else(|code| ServerMessage::Error {
      message: "invalid history page offset or size".into(),
      code,
    });
    write_before_deadline(&mut self.attachment.writer, &message, self.deadline)
      .await
      .map(|written| written.is_some())
  }

  fn renew_liveness(&mut self) {
    // Once the child exits, acknowledgements may drain its final output but
    // must not extend the deadline. Heartbeats alone cannot keep an ended
    // session's attachment, or the daemon, alive indefinitely.
    if self.pending_session_end.is_none() {
      self.deadline = Instant::now() + self.attachment_liveness_timeout;
    }
  }

  async fn send_session_end_if_drained(&mut self) -> Result<bool, ConnectionError> {
    let Some(ended) = self.pending_session_end else {
      return Ok(false);
    };
    if self.attachment.sent_sequence < ended.final_sequence {
      return Ok(false);
    }
    if self
      .attachment
      .pinned_history
      .as_ref()
      .is_some_and(|history| !history.complete() && !history.expired())
    {
      return Ok(false);
    }

    // Once every final byte is represented by queued output or a checkpoint,
    // the peer can drain those frames after we close. Its acknowledgement of
    // that last presentation is unnecessary for ordered end delivery.
    let shell_state = self.session.shell_state_for_attachment(
      &self.attachment_id,
      self.attachment.request_command_line,
      self.attachment.request_running_command,
    );
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
      return Ok(true);
    }
    let message = ServerMessage::SessionEnded {
      session_id: self.session.info().session_id,
      exit_code: ended.exit_code,
    };
    write_before_deadline(&mut self.attachment.writer, &message, self.deadline)
      .await
      .map(|_| true)
  }

  fn accept_presentation_progress(&mut self, sequence: u64) -> Result<(), &'static str> {
    if sequence > self.attachment.sent_sequence {
      return Err("presentation acknowledgement is ahead of sent output");
    }
    if let Some(applied_sequence) = self.attachment.applied_sequence {
      if sequence < applied_sequence {
        return Err("presentation acknowledgement regressed");
      }
      if sequence == applied_sequence {
        return Ok(());
      }
      if !self
        .attachment
        .in_flight_output
        .iter()
        .any(|delivery| delivery.sequence_end == sequence)
      {
        return Err("presentation acknowledgement is not an output frame boundary");
      }
    } else if sequence != self.attachment.sent_sequence
      || !self.attachment.in_flight_output.is_empty()
    {
      return Err("presentation acknowledgement does not match the pending checkpoint");
    }

    self.attachment.applied_sequence = Some(sequence);
    while self
      .attachment
      .in_flight_output
      .front()
      .is_some_and(|delivery| delivery.sequence_end <= sequence)
    {
      let delivery = self
        .attachment
        .in_flight_output
        .pop_front()
        .expect("front delivery exists");
      self.attachment.in_flight_charge_bytes = self
        .attachment
        .in_flight_charge_bytes
        .saturating_sub(delivery.charge_bytes);
    }
    Ok(())
  }

  async fn send_available_output(&mut self) -> Result<bool, ConnectionError> {
    if self.attachment.applied_sequence.is_none() {
      return Ok(true);
    }
    if self.attachment.pending_checkpoint && self.attachment.in_flight_output.is_empty() {
      let snapshot = self.session.fresh_snapshot()?;
      self.attachment.pending_checkpoint = false;
      return self.send_checkpoint(snapshot).await;
    }

    loop {
      let available_bytes = self
        .attachment
        .presentation_window_bytes
        .saturating_sub(self.attachment.in_flight_charge_bytes);
      if available_bytes == 0 {
        return Ok(true);
      }

      let mut snapshot = self.session.snapshot_for_delivery(
        Some(self.attachment.sent_sequence),
        self.attachment.checkpoint_geometry_revision,
      )?;
      if let Some(checkpoint) = snapshot.checkpoint {
        if !self.attachment.in_flight_output.is_empty() {
          return Ok(true);
        }
        snapshot.checkpoint = Some(checkpoint);
        return self.send_checkpoint(snapshot).await;
      }

      let frame_limit = usize::try_from(available_bytes)
        .unwrap_or(usize::MAX)
        .min(MAX_OUTPUT_FRAME_BYTES);
      let Some(chunk) = coalesce_output_chunks(
        snapshot.journal.chunks,
        self.attachment.sent_sequence,
        frame_limit,
      ) else {
        return Ok(true);
      };
      let sequence_end = chunk.sequence_end();
      let charge_bytes =
        output_frame_charge(chunk.data.len(), self.attachment.presentation_window_bytes);
      if charge_bytes > available_bytes {
        return Ok(true);
      }
      if write_before_deadline(
        &mut self.attachment.writer,
        &chunk.into_server_message(),
        self.deadline,
      )
      .await?
      .is_none()
      {
        return Ok(false);
      }
      self.attachment.sent_sequence = sequence_end;
      self.attachment.in_flight_charge_bytes += charge_bytes;
      self.attachment.in_flight_output.push_back(OutputDelivery {
        sequence_end,
        charge_bytes,
      });
    }
  }

  async fn send_checkpoint(
    &mut self,
    mut snapshot: AttachSnapshot,
  ) -> Result<bool, ConnectionError> {
    let pinned_history = pin_snapshot_history(&mut snapshot, self.attachment.protocol_version)?;
    let checkpoint = snapshot
      .checkpoint
      .expect("replacing snapshot has checkpoint");
    let sequence = checkpoint.sequence;
    let history = snapshot
      .history
      .expect("replacing snapshot has paired history");
    let message = ServerMessage::Checkpoint {
      checkpoint,
      history: Box::new(history),
      history_manifest: pinned_history
        .as_ref()
        .map(|history| Box::new(history.manifest.clone())),
      history_gap: snapshot.history_gap,
    };
    if write_before_deadline(&mut self.attachment.writer, &message, self.deadline)
      .await?
      .is_none()
    {
      return Ok(false);
    }
    self.attachment.pinned_history = pinned_history;
    // The new manifest invalidates the previous transfer. Do not let an
    // obsolete queued request occupy the new snapshot's single page slot.
    self.attachment.pending_history = None;
    self.attachment.sent_sequence = sequence;
    self.attachment.applied_sequence = None;
    self.attachment.checkpoint_geometry_revision = snapshot.checkpoint_geometry_revision;
    Ok(true)
  }

  async fn process_session_event(
    &mut self,
    event: Result<SessionEvent, broadcast::error::RecvError>,
  ) -> Result<bool, ConnectionError> {
    match event {
      Ok(SessionEvent::Output) => self.send_available_output().await,
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
        // The PTY reader publishes every byte before Ended. Retain its stable
        // final boundary while flow control waits for presentation credit.
        self.pending_session_end = Some(PendingSessionEnd {
          exit_code,
          final_sequence: self.session.info().next_sequence,
        });
        self.send_available_output().await
      }
      Err(broadcast::error::RecvError::Lagged(_)) => {
        if !self.send_available_output().await? {
          return Ok(false);
        }
        let shell_state = self.session.shell_state_for_attachment(
          &self.attachment_id,
          self.attachment.request_command_line,
          self.attachment.request_running_command,
        );
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
    terminal_size: ctmux_proto::TerminalSize,
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

    // `Terminal::resize` serializes this event with output publication, so a
    // live attachment can only see it at its next raw-output offset. Treat an
    // impossible gap as a recovery boundary instead of allowing a renderer to
    // resize ahead of unrendered raw output.
    if observed_sequence > self.attachment.sent_sequence {
      return self.send_available_output().await;
    }

    // Reflow may pull remote scrollback into the live grid. A renderer with
    // only a recent tail cannot reproduce this by resizing its local grid.
    let _ = terminal_size;
    self.send_available_output().await
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
      self.attachment.request_running_command && self.session.owns_input_lease(&self.attachment_id),
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
  client_terminal_size: ctmux_proto::TerminalSize,
  attachment_leases: ctmux_core::AttachmentLeases,
  attachment_token: String,
  shell_state: ShellState,
  history_manifest: Option<TerminalHistoryManifest>,
) -> Result<(), ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  let terminal_size_mismatch = snapshot.session.terminal_size != client_terminal_size;
  write_frame(
    writer,
    &ServerMessage::Attached {
      attachment_token,
      session: snapshot.session,
      earliest_sequence: snapshot.journal.earliest_sequence,
      next_sequence: snapshot.journal.next_sequence,
      replay_from: snapshot.journal.replay_from,
      history_gap: snapshot.history_gap,
      checkpoint: snapshot.checkpoint,
      history: snapshot.history.map(Box::new),
      history_manifest: history_manifest.map(Box::new),
      terminal_size_mismatch,
      input_lease: attachment_leases.input,
      layout_lease: attachment_leases.layout,
      shell_state,
    },
  )
  .await?;
  Ok(())
}

fn shell_state_for_attachment(
  shell_state: ShellState,
  may_view_command_line: bool,
  may_view_running_command: bool,
) -> ShellState {
  shell_state.filtered_for_visibility(may_view_command_line, may_view_running_command)
}

async fn process_attach_input<W>(
  writer: &mut W,
  session: Arc<Terminal>,
  attachment_id: &str,
  protocol_version: ctl_core::protocol::ProtocolVersion,
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
    ClientMessage::SetViewZoom { terminal_id } => {
      process_view_zoom(
        writer,
        session,
        attachment_id,
        protocol_version,
        terminal_id,
      )
      .await?;
    }
    request @ (ClientMessage::ResizePane { .. } | ClientMessage::ResizeDivider { .. }) => {
      process_pane_resize(writer, session, attachment_id, protocol_version, request).await?;
    }
    request @ (ClientMessage::SwapPane { .. } | ClientMessage::BreakPane { .. }) => {
      process_pane_move(writer, session, attachment_id, protocol_version, request).await?;
    }
    ClientMessage::Heartbeat { nonce } => {
      write_frame(writer, &ServerMessage::HeartbeatAck { nonce }).await?;
    }
    ClientMessage::Detach => return Ok(false),
    _ => {
      send_error(
        writer,
        ErrorCode::InvalidRequest,
        "only input, resize, pane moves, zoom, lease control, and detach are valid while attached",
      )
      .await?;
    }
  }
  Ok(true)
}

async fn process_view_zoom<W>(
  writer: &mut W,
  session: Arc<Terminal>,
  attachment_id: &str,
  protocol_version: ctl_core::protocol::ProtocolVersion,
  terminal_id: Option<String>,
) -> Result<(), ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  if !ctmux_proto::supports_view_zoom(protocol_version) {
    send_error(
      writer,
      ErrorCode::InvalidRequest,
      "view zoom requires contract 1.1.15",
    )
    .await?;
    return Ok(());
  }
  let attachment_id = attachment_id.to_owned();
  let result =
    tokio::task::spawn_blocking(move || session.set_view_zoom(&attachment_id, terminal_id)).await?;
  match result {
    Ok(view) => {
      write_frame(
        writer,
        &ServerMessage::ViewSnapshot {
          view: view_for_contract(view, protocol_version),
        },
      )
      .await?;
    }
    Err(error) => send_control_error(writer, &error).await?,
  }
  Ok(())
}

struct PaneResizeRequest {
  request_id: String,
  action: PaneResizeAction,
}

enum PaneResizeAction {
  Pane {
    terminal_id: String,
    direction: ctmux_proto::ResizeDirection,
    amount: u16,
  },
  Divider(ctmux_proto::DividerResize),
}

async fn process_pane_resize<W>(
  writer: &mut W,
  session: Arc<Terminal>,
  attachment_id: &str,
  protocol_version: ctl_core::protocol::ProtocolVersion,
  request: ClientMessage,
) -> Result<(), ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  let request = match request {
    ClientMessage::ResizePane {
      request_id,
      terminal_id,
      direction,
      amount,
    } => PaneResizeRequest {
      request_id,
      action: PaneResizeAction::Pane {
        terminal_id,
        direction,
        amount,
      },
    },
    ClientMessage::ResizeDivider {
      request_id,
      divider,
    } => PaneResizeRequest {
      request_id,
      action: PaneResizeAction::Divider(divider),
    },
    _ => {
      send_error(
        writer,
        ErrorCode::InvalidRequest,
        "expected pane or divider resize",
      )
      .await?;
      return Ok(());
    }
  };
  if !ctmux_proto::supports_pane_resize(protocol_version) {
    send_error(
      writer,
      ErrorCode::InvalidRequest,
      "pane resize requires contract 1.1.16",
    )
    .await?;
    return Ok(());
  }
  let outcome = if matches!(&request.action, PaneResizeAction::Divider(_))
    && !ctmux_proto::supports_divider_resize(protocol_version)
  {
    ctmux_proto::PaneResizeOutcome::Rejected {
      code: ErrorCode::InvalidRequest,
      message: "divider resize requires contract 1.1.17".into(),
    }
  } else if request.request_id.is_empty()
    || request.request_id.len() > ctmux_proto::MAX_PANE_RESIZE_REQUEST_ID_BYTES
  {
    ctmux_proto::PaneResizeOutcome::Rejected {
      code: ErrorCode::InvalidRequest,
      message: "pane resize request_id must contain 1 to 256 bytes".into(),
    }
  } else {
    let attachment_id = attachment_id.to_owned();
    let result = tokio::task::spawn_blocking(move || match request.action {
      PaneResizeAction::Pane {
        terminal_id,
        direction,
        amount,
      } => session.resize_pane(&attachment_id, &terminal_id, direction, amount),
      PaneResizeAction::Divider(divider) => session.resize_divider(&attachment_id, &divider),
    })
    .await?;
    match result {
      Ok(view) => ctmux_proto::PaneResizeOutcome::Applied {
        view: Box::new(view),
      },
      Err(error) => ctmux_proto::PaneResizeOutcome::Rejected {
        code: control_error_code(&error),
        message: error.to_string(),
      },
    }
  };
  write_frame(
    writer,
    &ServerMessage::PaneResizeResult {
      request_id: request.request_id,
      outcome,
    },
  )
  .await?;
  Ok(())
}

enum PaneMoveAction {
  Swap { previous: bool },
  Break { name: Option<String> },
}

async fn process_pane_move<W>(
  writer: &mut W,
  session: Arc<Terminal>,
  attachment_id: &str,
  protocol_version: ctl_core::protocol::ProtocolVersion,
  request: ClientMessage,
) -> Result<(), ConnectionError>
where
  W: tokio::io::AsyncWrite + Unpin,
{
  let (request_id, target, action) = match request {
    ClientMessage::SwapPane {
      request_id,
      target,
      previous,
    } => (request_id, target, PaneMoveAction::Swap { previous }),
    ClientMessage::BreakPane {
      request_id,
      target,
      name,
    } => (request_id, target, PaneMoveAction::Break { name }),
    _ => {
      send_error(writer, ErrorCode::InvalidRequest, "expected pane move").await?;
      return Ok(());
    }
  };
  if !ctmux_proto::supports_pane_moves(protocol_version) {
    send_error(
      writer,
      ErrorCode::InvalidRequest,
      "pane moves require contract 1.1.19",
    )
    .await?;
    return Ok(());
  }
  let outcome =
    if request_id.is_empty() || request_id.len() > ctmux_proto::MAX_PANE_MOVE_REQUEST_ID_BYTES {
      ctmux_proto::PaneMoveOutcome::Rejected {
        code: ErrorCode::InvalidRequest,
        message: "pane move request_id must contain 1 to 256 bytes".into(),
      }
    } else {
      let attachment_id = attachment_id.to_owned();
      let result = tokio::task::spawn_blocking(move || match action {
        PaneMoveAction::Swap { previous } => session
          .swap_pane(&attachment_id, &target, previous)
          .map(|view| ctmux_proto::PaneMoveOutcome::Swapped {
            view: Box::new(view),
          })
          .map_err(PaneMoveError::from),
        PaneMoveAction::Break { name } => {
          session
            .break_pane(&attachment_id, &target, name)
            .map(
              |(view, source_view)| ctmux_proto::PaneMoveOutcome::Promoted {
                view: Box::new(view),
                source_view: Box::new(source_view),
              },
            )
        }
      })
      .await?;
      match result {
        Ok(outcome) => outcome,
        Err(error) => ctmux_proto::PaneMoveOutcome::Rejected {
          code: match &error {
            PaneMoveError::Control(error) => control_error_code(error),
            PaneMoveError::Manager(error) => session_manager_error_code(error),
          },
          message: error.to_string(),
        },
      }
    };
  write_frame(
    writer,
    &ServerMessage::PaneMoveResult {
      request_id,
      outcome,
    },
  )
  .await?;
  Ok(())
}

fn renews_attachment_liveness(message: &ClientMessage) -> bool {
  matches!(
    message,
    ClientMessage::Input { .. }
      | ClientMessage::Resize { .. }
      | ClientMessage::SetViewZoom { .. }
      | ClientMessage::ResizePane { .. }
      | ClientMessage::ResizeDivider { .. }
      | ClientMessage::SwapPane { .. }
      | ClientMessage::BreakPane { .. }
      | ClientMessage::AcquireLease { .. }
      | ClientMessage::ReleaseLease { .. }
      | ClientMessage::Heartbeat { .. }
      | ClientMessage::HistoryRequest { .. }
      | ClientMessage::RequestCheckpoint
      | ClientMessage::Detach
  )
}

fn initial_attachment_delivery_deadline() -> Instant {
  Instant::now() + INITIAL_ATTACHMENT_DELIVERY_TIMEOUT
}

fn output_frame_charge(data_len: usize, presentation_window_bytes: u64) -> u64 {
  let minimum_charge = presentation_window_bytes.min(MIN_OUTPUT_FRAME_CHARGE_BYTES);
  u64::try_from(data_len)
    .unwrap_or(u64::MAX)
    .max(minimum_charge)
}

fn coalesce_output_chunks(
  chunks: Vec<OutputChunk>,
  sequence: u64,
  max_bytes: usize,
) -> Option<OutputChunk> {
  if max_bytes == 0 {
    return None;
  }

  let mut data = Vec::with_capacity(max_bytes.min(MAX_OUTPUT_FRAME_BYTES));
  let mut next_sequence = sequence;
  for chunk in chunks {
    let Some(chunk) = chunk_after(chunk, next_sequence) else {
      continue;
    };
    if chunk.sequence_start != next_sequence {
      break;
    }
    let remaining = max_bytes - data.len();
    let take = remaining.min(chunk.data.len());
    data.extend_from_slice(&chunk.data[..take]);
    next_sequence += take as u64;
    if data.len() == max_bytes {
      break;
    }
  }

  (!data.is_empty()).then_some(OutputChunk {
    sequence_start: sequence,
    data,
  })
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
  stream: &mut Stream,
  error: &SessionManagerError,
) -> Result<(), CodecError> {
  send_error(
    stream,
    session_manager_error_code(error),
    &error.to_string(),
  )
  .await
}

fn session_manager_error_code(error: &SessionManagerError) -> ErrorCode {
  match error {
    SessionManagerError::InvalidView(_) => ErrorCode::InvalidRequest,
    SessionManagerError::InvalidName { .. } => ErrorCode::InvalidSessionName,
    SessionManagerError::AlreadyExists { .. } => ErrorCode::SessionAlreadyExists,
    SessionManagerError::NotFound { .. } => ErrorCode::SessionNotFound,
    #[cfg(unix)]
    SessionManagerError::ShellReporter(_) => ErrorCode::Internal,
    SessionManagerError::Pty(_)
    | SessionManagerError::Spawn(_)
    | SessionManagerError::ReaderThread(_)
    | SessionManagerError::WaiterThread(_)
    | SessionManagerError::AutomaticNameExhausted => ErrorCode::Internal,
  }
}

async fn send_journal_error(stream: &mut Stream, error: &JournalError) -> Result<(), CodecError> {
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
  let code = control_error_code(error);
  send_error(writer, code, &error.to_string()).await
}

fn control_error_code(error: &SessionControlError) -> ErrorCode {
  match error {
    SessionControlError::InvalidView(_) => ErrorCode::InvalidRequest,
    SessionControlError::InputLeaseRequired => ErrorCode::InputLeaseRequired,
    SessionControlError::LayoutLeaseRequired => ErrorCode::LayoutLeaseRequired,
    SessionControlError::Io(_) | SessionControlError::Pty(_) => ErrorCode::Internal,
  }
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
  #[error("history snapshot I/O failed: {0}")]
  History(#[from] io::Error),
  #[error(transparent)]
  Codec(#[from] CodecError),
  #[error(transparent)]
  LocalControl(#[from] ctmux_ipc::LocalControlCodecError),
  #[error("local-control request timed out")]
  LocalControlTimeout,
  #[error(transparent)]
  Journal(#[from] JournalError),
  #[error(transparent)]
  Control(#[from] SessionControlError),
  #[error("blocking daemon task failed: {0}")]
  Task(#[from] tokio::task::JoinError),
}

impl ConnectionError {
  fn diagnostic_code(&self) -> &'static str {
    match self {
      Self::History(_) => "history_io_failed",
      Self::Codec(_) => "session_protocol_failed",
      Self::LocalControl(_) => "control_protocol_failed",
      Self::LocalControlTimeout => "control_request_timeout",
      Self::Journal(_) => "terminal_journal_failed",
      Self::Control(_) => "terminal_control_failed",
      Self::Task(_) => "daemon_worker_failed",
    }
  }
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
  session: Arc<Terminal>,
  attachment_token: String,
  generation: u64,
  reconnect_grace: Duration,
  preserve_on_drop: bool,
  active: bool,
}

impl AttachmentGuard {
  fn close_now(&mut self) {
    if self.active {
      self
        .session
        .close_attachment(&self.attachment_token, self.generation);
      self.active = false;
    }
  }
}

impl Drop for AttachmentGuard {
  fn drop(&mut self) {
    if !self.active {
      return;
    }
    if !self.preserve_on_drop {
      self
        .session
        .close_attachment(&self.attachment_token, self.generation);
      return;
    }
    if !self
      .session
      .suspend_attachment(&self.attachment_token, self.generation)
    {
      return;
    }

    let session = Arc::clone(&self.session);
    let attachment_token = self.attachment_token.clone();
    let generation = self.generation;
    let reconnect_grace = self.reconnect_grace;
    tokio::spawn(async move {
      tokio::time::sleep(reconnect_grace).await;
      session.expire_attachment(&attachment_token, generation);
    });
  }
}

impl Drop for ConnectionGuard {
  fn drop(&mut self) {
    self.tracker.active.fetch_sub(1, Ordering::AcqRel);
    self.tracker.changed.notify_one();
  }
}

#[cfg(unix)]
struct SocketGuard {
  path: PathBuf,
  device: u64,
  inode: u64,
}

#[cfg(unix)]
struct EndpointStartupLock {
  _file: std::fs::File,
}

#[cfg(unix)]
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

#[cfg(unix)]
fn endpoint_startup_lock_path(socket_path: &Path) -> PathBuf {
  let mut path = socket_path.as_os_str().to_os_string();
  path.push(".lock");
  path.into()
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use ctmux_core::JournalSnapshot;
  use ctmux_proto::{LeaseStatus, SessionStatus, TerminalHistoryRow, normalize_history_rows};
  use sha2::{Digest, Sha256};
  use tokio::time::timeout;

  fn history_fixture(rows: &[TerminalHistoryRow]) -> TerminalHistorySnapshot {
    let lines = normalize_history_rows(rows);
    TerminalHistorySnapshot {
      format: ctmux_proto::TERMINAL_HISTORY_FORMAT.into(),
      format_version: 1,
      sequence: 42,
      generation: 3,
      revision: 7,
      retained_bytes: lines.iter().map(|line| (line.len() + 1) as u64).sum(),
      truncated: false,
      lines,
    }
  }

  #[test]
  fn historical_contracts_omit_only_the_captured_keyboard_preamble() {
    let prefix = b"\x1b[>4;2m";
    // The same bytes inside an OSC string or a pending CSI are application
    // state, not the daemon's preamble, and must survive the projection.
    let legacy = b"\x1b[?2004h\x1b]0;\x1b[>4;2m\x07\x1b[>4;";
    let payload = [prefix.as_slice(), legacy.as_slice()].concat();
    let base = AttachSnapshot {
      session: ctmux_proto::SessionInfo {
        view_id: "view".into(),
        terminal_id: "terminal".into(),
        session_id: "session".into(),
        name: "work".into(),
        status: SessionStatus::Running,
        created_at_ms: 0,
        next_sequence: 42,
        terminal_size: ctmux_proto::TerminalSize::default(),
      },
      checkpoint: Some(ctmux_proto::TerminalCheckpoint {
        format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
        format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
        sequence: 42,
        terminal_size: ctmux_proto::TerminalSize::default(),
        payload: payload.clone(),
        input_prefix: vec![0xe2],
      }),
      checkpoint_keyboard_prefix_bytes: prefix.len(),
      checkpoint_geometry_revision: Some(3),
      journal: JournalSnapshot {
        earliest_sequence: 0,
        next_sequence: 42,
        replay_from: 42,
        history_gap: false,
        chunks: Vec::new(),
      },
      history_gap: false,
      history: None,
      history_rows: None,
      scrollback_limit: 2_000,
      shell_state: ShellState::default(),
    };
    for &version in ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS {
      let mut snapshot = base.clone();
      project_checkpoint_for_contract(&mut snapshot, version);
      project_checkpoint_for_contract(&mut snapshot, version);
      let checkpoint = snapshot.checkpoint.unwrap();
      assert_eq!(
        checkpoint.payload,
        if ctmux_proto::supports_extended_keys(version) {
          payload.as_slice()
        } else {
          legacy.as_slice()
        },
        "contract {version}"
      );
      assert_eq!(checkpoint.input_prefix, [0xe2]);
      assert_eq!(checkpoint.sequence, 42);
    }
  }

  #[test]
  fn cached_history_body_keeps_transfer_metadata_and_progress_independent() {
    for rows in [
      Vec::new(),
      vec![TerminalHistoryRow {
        text: "unchanged".into(),
        wrapped: false,
      }],
    ] {
      let physical = PhysicalHistory::new(rows.clone());
      let original_history = history_fixture(&rows);
      let (mut old, _) = PinnedHistory::new(&original_history, &physical, 10_000).unwrap();
      let mut new_history = original_history.clone();
      new_history.sequence += 1;
      new_history.generation += 1;
      new_history.revision += 1;
      new_history.truncated = true;
      let (new, _) = PinnedHistory::new(&new_history, &physical, 250).unwrap();
      assert!(Arc::ptr_eq(&old.data, &new.data));
      assert_eq!(old.manifest.content_hash, new.manifest.content_hash);
      assert_ne!(old.manifest.snapshot_id, new.manifest.snapshot_id);
      assert_eq!(new.manifest.sequence, new_history.sequence);
      assert_eq!(new.manifest.generation, new_history.generation);
      assert_eq!(new.manifest.revision, new_history.revision);
      assert!(new.manifest.truncated);
      assert_eq!(new.manifest.scrollback_limit, 250);
      old
        .page(&HistoryRequest {
          snapshot_id: old.manifest.snapshot_id.clone(),
          offset: 0,
          max_bytes: 5,
        })
        .unwrap();
      assert_eq!(new.served_end, 0);
      old.last_access = Instant::now() - HISTORY_SNAPSHOT_IDLE_TTL;
      assert!(old.expired());
      assert!(!new.expired());
    }
  }

  #[test]
  fn history_pages_are_byte_bounded_and_preserve_long_unicode_wrapped_rows() {
    let rows = vec![
      TerminalHistoryRow {
        text: "界\"".repeat(20_000),
        wrapped: true,
      },
      TerminalHistoryRow {
        text: "tail   ".into(),
        wrapped: false,
      },
      TerminalHistoryRow {
        text: "partial".into(),
        wrapped: true,
      },
    ];
    let physical = PhysicalHistory::new(rows.clone());
    let (mut pinned, recent) =
      PinnedHistory::new(&history_fixture(&rows), &physical, 10_000).unwrap();
    assert_eq!(pinned.manifest.total_rows, 3);
    assert_eq!(pinned.manifest.total_lines, 1);
    assert!(
      recent.lines.is_empty(),
      "a huge recent line must not block the screen frame"
    );
    let mut data = Vec::new();
    let mut offset = 0;
    loop {
      let request = HistoryRequest {
        snapshot_id: pinned.manifest.snapshot_id.clone(),
        offset,
        max_bytes: u64::MAX,
      };
      let ServerMessage::HistoryPage {
        offset: actual,
        data: page,
        next_offset,
        ..
      } = pinned.page(&request).unwrap()
      else {
        panic!("expected history page")
      };
      assert_eq!(actual, offset);
      assert!(page.len() <= MAX_HISTORY_PAGE_BYTES);
      data.extend(page);
      if let Some(next) = next_offset {
        offset = next;
      } else {
        break;
      }
    }
    assert!(pinned.complete());
    assert_eq!(data.len() as u64, pinned.manifest.total_bytes);
    assert_eq!(
      format!("{:x}", Sha256::digest(&data)),
      pinned.manifest.content_hash
    );
    let decoded: Vec<TerminalHistoryRow> = data
      .split(|byte| *byte == b'\n')
      .filter(|line| !line.is_empty())
      .map(|line| serde_json::from_slice(line).unwrap())
      .collect();
    assert_eq!(decoded, rows);
  }

  #[test]
  fn history_snapshot_renews_idle_expiry_and_rejects_stale_ids_and_bounds() {
    let rows: Vec<_> = (0..100)
      .map(|index| TerminalHistoryRow {
        text: format!("line {index}"),
        wrapped: false,
      })
      .collect();
    let physical = PhysicalHistory::new(rows.clone());
    let (mut pinned, recent) =
      PinnedHistory::new(&history_fixture(&rows), &physical, 10_000).unwrap();
    assert_eq!(recent.lines.len(), MAX_RECENT_HISTORY_LINES);
    assert_eq!(pinned.manifest.first_line, 36);
    let mut request = HistoryRequest {
      snapshot_id: pinned.manifest.snapshot_id.clone(),
      offset: 0,
      max_bytes: 1,
    };
    pinned.last_access = Instant::now() - HISTORY_SNAPSHOT_IDLE_TTL / 2;
    pinned.page(&request).unwrap();
    assert!(pinned.last_access.elapsed() < Duration::from_secs(1));
    request.offset = pinned.manifest.total_bytes + 1;
    assert_eq!(
      pinned.page(&request).unwrap_err(),
      ErrorCode::InvalidRequest
    );
    request.offset = 0;
    request.max_bytes = 0;
    assert_eq!(
      pinned.page(&request).unwrap_err(),
      ErrorCode::InvalidRequest
    );
    request.max_bytes = 1;
    request.snapshot_id = "replaced".into();
    assert_eq!(
      pinned.page(&request).unwrap(),
      ServerMessage::HistorySnapshotExpired {
        snapshot_id: request.snapshot_id.clone()
      }
    );
    request.snapshot_id.clone_from(&pinned.manifest.snapshot_id);
    pinned.last_access = Instant::now() - HISTORY_SNAPSHOT_IDLE_TTL;
    assert_eq!(
      pinned.page(&request).unwrap(),
      ServerMessage::HistorySnapshotExpired {
        snapshot_id: request.snapshot_id.clone()
      }
    );
  }

  #[test]
  fn fetching_only_the_last_page_does_not_complete_history_drain() {
    let rows = vec![TerminalHistoryRow {
      text: "last".into(),
      wrapped: false,
    }];
    let physical = PhysicalHistory::new(rows.clone());
    let (mut pinned, _) = PinnedHistory::new(&history_fixture(&rows), &physical, 10_000).unwrap();
    let request = HistoryRequest {
      snapshot_id: pinned.manifest.snapshot_id.clone(),
      offset: pinned.manifest.total_bytes - 1,
      max_bytes: 1,
    };
    pinned.page(&request).unwrap();
    assert!(!pinned.complete());
  }

  #[test]
  fn endpoint_startup_lock_is_exclusive_and_owner_only() {
    use rustix::fs::{FlockOperation, flock};
    use std::os::unix::fs::PermissionsExt as _;

    let directory = std::env::temp_dir().join(format!("ctmux-start-lock-{}", Uuid::new_v4()));
    let cleanup = TestDirectory(directory.clone());
    let socket_path = directory.join("ctmux.sock");
    ctmux_ipc::prepare_runtime_directory(&socket_path).unwrap();

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

  #[test]
  fn armed_local_control_request_outlasts_gui_detach_window() {
    // The GUI waits up to five seconds for detach before using its armed
    // control stream. Keep this assertion beside the endpoint timeout so a
    // future change cannot silently reintroduce the preflight/detach race.
    assert!(LOCAL_CONTROL_ARMED_REQUEST_TIMEOUT > Duration::from_secs(5));
  }

  struct TestDirectory(PathBuf);

  impl Drop for TestDirectory {
    fn drop(&mut self) {
      let _ignored = std::fs::remove_dir_all(&self.0);
    }
  }

  #[tokio::test]
  async fn initial_delivery_defers_replay_until_presentation_credit() {
    let replay = vec![b'x'; 4 * 1024];
    let next_sequence = u64::try_from(replay.len()).expect("test replay fits in u64");
    let snapshot = AttachSnapshot {
      session: ctmux_proto::SessionInfo {
        view_id: "view-test".into(),
        terminal_id: "terminal-test".into(),
        session_id: "session-id".into(),
        name: "session".into(),
        status: SessionStatus::Running,
        created_at_ms: 0,
        next_sequence,
        terminal_size: ctmux_proto::TerminalSize::default(),
      },
      checkpoint: None,
      checkpoint_keyboard_prefix_bytes: 0,
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
      history: None,
      history_rows: None,
      scrollback_limit: 10_000,
      shell_state: ShellState::default(),
    };
    let attachment_leases = ctmux_core::AttachmentLeases {
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
          ctmux_proto::TerminalSize::default(),
          attachment_leases,
          "test-token".into(),
          ShellState::default(),
          None,
        ),
      )
      .await
    });

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
    timeout(Duration::from_secs(1), delivery)
      .await
      .expect("initial delivery did not finish")
      .expect("initial delivery task panicked")
      .expect("initial delivery hit its deadline")
      .expect("initial delivery failed");

    assert!(
      read_frame::<_, ServerMessage>(&mut reader)
        .await
        .expect("initial delivery stream remained valid")
        .is_none()
    );
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

  #[test]
  fn fragmented_output_is_coalesced_into_a_bounded_frame() {
    let chunks = (0..512)
      .map(|sequence_start| OutputChunk {
        sequence_start,
        data: b"x".to_vec(),
      })
      .collect();

    let frame = coalesce_output_chunks(chunks, 0, MAX_OUTPUT_FRAME_BYTES)
      .expect("fragmented output produced a frame");

    assert_eq!(frame.sequence_start, 0);
    assert_eq!(frame.sequence_end(), 512);
    assert_eq!(frame.data, vec![b'x'; 512]);
    assert_eq!(
      output_frame_charge(frame.data.len(), 256 * 1024),
      MIN_OUTPUT_FRAME_CHARGE_BYTES
    );
  }
}

#[cfg(windows)]
fn bind_daemon_endpoints(data: &Path, control: &Path) -> Result<DaemonEndpoints, DaemonError> {
  // The exclusive control instance arbitrates startup. Publish data last.
  // No filesystem lock or stale pipe removal is needed on Windows.
  let control_listener = Listener::bind(control).map_err(|source| DaemonError::Bind {
    path: control.into(),
    source,
  })?;
  let listener = Listener::bind(data).map_err(|source| DaemonError::Bind {
    path: data.into(),
    source,
  })?;
  Ok(DaemonEndpoints {
    listener,
    control_listener,
  })
}
