use crate::{
  ConnectFuture, Error, Result, Stream, Transport,
  app::App,
  input,
  pane::{Pane, ReconnectLeases},
};
use ctmux_proto::{
  ClientMessage, LeaseStatus, ServerMessage, SessionInfo, SessionStatus, ShellState, TerminalSize,
  read_frame, write_frame,
};
use std::{
  fmt, io,
  path::PathBuf,
  sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
  },
  task::Poll,
  time::Duration,
};
use tokio::{io::DuplexStream, time::Instant};

const HOST: &str = "clouds-modu deadline fixture";
const PREPARATION_DELAY: Duration = Duration::from_secs(8);

struct DelayedTransport {
  result: Mutex<Option<Result<Stream>>>,
}

impl DelayedTransport {
  fn stream(stream: DuplexStream) -> Self {
    Self {
      result: Mutex::new(Some(Ok(Box::new(stream)))),
    }
  }

  fn failure(error: Error) -> Self {
    Self {
      result: Mutex::new(Some(Err(error))),
    }
  }
}

impl Transport for DelayedTransport {
  fn connect(&self) -> ConnectFuture<'_> {
    Box::pin(async move {
      tokio::time::sleep(PREPARATION_DELAY).await;
      self
        .result
        .lock()
        .expect("fixture result lock")
        .take()
        .expect("one startup connection")
    })
  }

  fn archive_key(&self) -> String {
    "deadline-fixture".into()
  }

  fn label(&self) -> &'static str {
    HOST
  }

  fn is_local(&self) -> bool {
    false
  }
}

fn app(transport: &dyn Transport) -> App<'_> {
  let mut app = App::new(PathBuf::new(), true, input::parse_prefix("Ctrl+b").unwrap());
  app.transport = Some(transport);
  app
}

async fn receive_handshake(peer: &mut DuplexStream) {
  assert!(matches!(
    read_frame::<_, ClientMessage>(peer).await.unwrap(),
    Some(ClientMessage::Handshake { client_name, .. }) if client_name == "ctmux-tui"
  ));
}

async fn accept_handshake(peer: &mut DuplexStream, attachment_liveness_timeout_ms: u64) {
  receive_handshake(peer).await;
  write_frame(
    &mut *peer,
    &ServerMessage::HandshakeAccepted {
      protocol_version: ctmux_proto::PROTOCOL_VERSION,
      protocols: vec![ctmux_proto::protocol_info()],
      server_version: "deadline-fixture".into(),
      build: None,
      heartbeat_interval_ms: 1_000,
      attachment_liveness_timeout_ms,
    },
  )
  .await
  .unwrap();
}

async fn receive_list_request(peer: &mut DuplexStream) {
  accept_handshake(peer, 3_000).await;
  assert_eq!(
    read_frame::<_, ClientMessage>(peer).await.unwrap(),
    Some(ClientMessage::ListSessions)
  );
}

#[tokio::test(start_paused = true)]
async fn resume_startup_gives_protocol_its_own_budget_after_slow_preparation() -> Result<()> {
  let (stream, mut peer) = tokio::io::duplex(4096);
  let transport = DelayedTransport::stream(stream);
  let server = tokio::spawn(async move {
    receive_list_request(&mut peer).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    write_frame(
      &mut peer,
      &ServerMessage::SessionList {
        sessions: Vec::new(),
      },
    )
    .await
    .unwrap();
    assert!(
      read_frame::<_, ClientMessage>(&mut peer)
        .await
        .unwrap()
        .is_none()
    );
  });
  let started = Instant::now();
  app(&transport).start_resume().await?;
  assert_eq!(
    started.elapsed(),
    PREPARATION_DELAY + Duration::from_secs(3)
  );
  server.await?;
  Ok(())
}

async fn stalled_protocol_reports_context(handshake_stalls: bool) -> Result<()> {
  let (stream, mut peer) = tokio::io::duplex(4096);
  let transport = DelayedTransport::stream(stream);
  let server = tokio::spawn(async move {
    if handshake_stalls {
      receive_handshake(&mut peer).await;
    } else {
      receive_list_request(&mut peer).await;
    }
    // Keep the selected connection alive until the client cancels its stalled
    // protocol exchange. Returning early would test EOF instead of a deadline.
    assert!(
      read_frame::<_, ClientMessage>(&mut peer)
        .await
        .unwrap()
        .is_none()
    );
  });
  let started = Instant::now();
  let error = app(&transport).start_resume().await.unwrap_err();
  let message = error.to_string();
  assert!(message.contains(HOST), "missing target: {message}");
  assert!(
    message.contains("list sessions"),
    "missing operation: {message}"
  );
  assert!(message.contains("protocol"), "missing phase: {message}");
  assert!(
    !message.contains("deadline has elapsed"),
    "opaque error: {message}"
  );
  assert_eq!(
    started.elapsed(),
    PREPARATION_DELAY + crate::deadline::REQUEST_TIMEOUT
  );
  server.await?;
  Ok(())
}

#[tokio::test(start_paused = true)]
async fn stalled_startup_handshake_reports_target_operation_and_protocol_phase() -> Result<()> {
  stalled_protocol_reports_context(true).await
}

#[tokio::test(start_paused = true)]
async fn stalled_session_list_reply_reports_target_operation_and_protocol_phase() -> Result<()> {
  stalled_protocol_reports_context(false).await
}

#[tokio::test(start_paused = true)]
async fn stalled_initial_checkpoint_reports_preparation_phase_and_closes_attachment() -> Result<()>
{
  let (stream, mut peer) = tokio::io::duplex(4096);
  let transport = DelayedTransport::stream(stream);
  let server = tokio::spawn(async move {
    // Keep attachment liveness above the checkpoint deadline. This fixture
    // stays responsive but deliberately never delivers the initial checkpoint.
    accept_handshake(&mut peer, 30_000).await;
    assert!(matches!(
      read_frame::<_, ClientMessage>(&mut peer).await.unwrap(),
      Some(ClientMessage::AttachSession { session, .. }) if session == "terminal-id"
    ));
    write_frame(
      &mut peer,
      &ServerMessage::Attached {
        attachment_token: "deadline-fixture-token".into(),
        session: SessionInfo {
          view_id: "view-id".into(),
          terminal_id: "terminal-id".into(),
          session_id: "session-id".into(),
          name: "checkpoint fixture".into(),
          status: SessionStatus::Running,
          created_at_ms: 0,
          next_sequence: 0,
          terminal_size: TerminalSize::default(),
        },
        earliest_sequence: 0,
        next_sequence: 0,
        replay_from: 0,
        history_gap: false,
        checkpoint: None,
        history: None,
        history_manifest: None,
        terminal_size_mismatch: false,
        input_lease: LeaseStatus {
          held: false,
          owned_by_client: false,
        },
        layout_lease: LeaseStatus {
          held: false,
          owned_by_client: false,
        },
        shell_state: ShellState::default(),
      },
    )
    .await
    .unwrap();
    while let Some(message) = read_frame::<_, ClientMessage>(&mut peer).await.unwrap() {
      match message {
        ClientMessage::Heartbeat { nonce } => {
          // Cancellation may close the stream between reading a heartbeat and
          // replying. Continue reading so EOF still proves controller cleanup.
          let _reply = write_frame(&mut peer, &ServerMessage::HeartbeatAck { nonce }).await;
        }
        ClientMessage::RequestCheckpoint => {}
        other => panic!("unexpected checkpoint fixture request: {other:?}"),
      }
    }
  });
  let started = Instant::now();
  let error = Pane::open(
    &transport,
    "terminal-id",
    TerminalSize::default(),
    ReconnectLeases::new(false, false),
    None,
    crate::deadline::ConnectionMode::Startup,
  )
  .await
  .err()
  .expect("an attachment without its initial checkpoint must time out");
  let message = error.to_string();
  assert!(message.contains(HOST), "missing target: {message}");
  assert!(
    message.contains("prepare terminal"),
    "missing operation: {message}"
  );
  assert!(
    message.contains("initial checkpoint and lease confirmation"),
    "missing phase: {message}"
  );
  assert_eq!(
    started.elapsed(),
    PREPARATION_DELAY + crate::deadline::REQUEST_TIMEOUT
  );
  server.await?;
  Ok(())
}

#[derive(Debug)]
struct AuthenticationRequired;

impl fmt::Display for AuthenticationRequired {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str("fixture host requires authentication")
  }
}

impl std::error::Error for AuthenticationRequired {}

#[tokio::test(start_paused = true)]
async fn slow_startup_authentication_failure_preserves_the_connector_error_type() {
  let transport = DelayedTransport::failure(Box::new(AuthenticationRequired));
  let started = Instant::now();
  let error = app(&transport).start_resume().await.unwrap_err();
  assert!(error.downcast_ref::<AuthenticationRequired>().is_some());
  assert_eq!(started.elapsed(), PREPARATION_DELAY);
}

#[tokio::test(start_paused = true)]
async fn slow_startup_io_failure_preserves_the_original_error_kind_and_message() {
  let transport = DelayedTransport::failure(Box::new(io::Error::new(
    io::ErrorKind::ConnectionRefused,
    "fixture SSH connection refused",
  )));
  let started = Instant::now();
  let error = app(&transport).start_resume().await.unwrap_err();
  let error = error
    .downcast_ref::<io::Error>()
    .expect("original I/O error");
  assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
  assert_eq!(error.to_string(), "fixture SSH connection refused");
  assert_eq!(started.elapsed(), PREPARATION_DELAY);
}

struct PendingTransport {
  started: AtomicUsize,
  cancelled: Arc<AtomicUsize>,
}

struct CancelledConnection(Arc<AtomicUsize>);

impl Drop for CancelledConnection {
  fn drop(&mut self) {
    self.0.fetch_add(1, Ordering::Relaxed);
  }
}

impl Transport for PendingTransport {
  fn connect(&self) -> ConnectFuture<'_> {
    Box::pin(async move {
      self.started.fetch_add(1, Ordering::Relaxed);
      let _cancelled = CancelledConnection(Arc::clone(&self.cancelled));
      std::future::pending::<Result<Stream>>().await
    })
  }

  fn archive_key(&self) -> String {
    "pending-deadline-fixture".into()
  }

  fn label(&self) -> &'static str {
    HOST
  }

  fn is_local(&self) -> bool {
    false
  }
}

#[tokio::test(start_paused = true)]
async fn cancelling_runtime_preparation_drops_the_underlying_connection_future() {
  let transport = PendingTransport {
    started: AtomicUsize::new(0),
    cancelled: Arc::default(),
  };
  let mut connection = Box::pin(crate::deadline::connect(
    &transport,
    "reconnect pane",
    crate::deadline::ConnectionMode::Runtime,
  ));
  std::future::poll_fn(|context| {
    assert!(connection.as_mut().poll(context).is_pending());
    Poll::Ready(())
  })
  .await;
  assert_eq!(transport.started.load(Ordering::Relaxed), 1);
  assert_eq!(transport.cancelled.load(Ordering::Relaxed), 0);
  drop(connection);
  assert_eq!(transport.cancelled.load(Ordering::Relaxed), 1);
}

#[tokio::test(start_paused = true)]
async fn runtime_preparation_deadline_reports_context_and_cancels_the_connection() {
  let transport = PendingTransport {
    started: AtomicUsize::new(0),
    cancelled: Arc::default(),
  };
  let started = Instant::now();
  let error = crate::deadline::connect(
    &transport,
    "reconnect pane",
    crate::deadline::ConnectionMode::Runtime,
  )
  .await
  .err()
  .expect("a stalled runtime connection must time out");
  let message = error.to_string();
  assert!(message.contains(HOST), "missing target: {message}");
  assert!(
    message.contains("reconnect pane"),
    "missing operation: {message}"
  );
  assert!(
    message.contains("connection preparation"),
    "missing phase: {message}"
  );
  assert!(
    !message.contains("deadline has elapsed"),
    "opaque error: {message}"
  );
  assert_eq!(started.elapsed(), crate::deadline::REQUEST_TIMEOUT);
  assert_eq!(transport.started.load(Ordering::Relaxed), 1);
  assert_eq!(transport.cancelled.load(Ordering::Relaxed), 1);
}
