#![cfg(unix)]

//! End-to-end lifetime coverage for the remote `rmux` service.
//!
//! This intentionally uses the real `rmuxd`, `ctld`, pinned control client,
//! and reusable `rmux-client` protocol API.  The release marker makes the
//! delayed shell output causally occur only after the first `ctld` has fully
//! stopped, rather than merely hoping a timed delay is long enough.

use ctl_core::{ClientIdentity as ControlIdentity, open_rmux_tunnel, pair};
use rmux_client::{
  AttachRequest, AttachedSession, ClientIdentity as RmuxIdentity, begin_attach,
  request as rmux_request,
};
use rmux_proto::{
  ClientMessage, CodecError, CommandSpec, ErrorCode, LeaseKind, LeaseStatus, ServerMessage,
  SessionInfo, SessionStatus, TerminalSize, read_frame, write_frame,
};
use rmuxd::{DaemonConfig as RmuxDaemonConfig, run as run_rmuxd};
use std::error::Error;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};
use uuid::Uuid;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const RMUX_CLIENT_NAME: &str = "ctld-remote-session-test";
const RMUX_CLIENT_VERSION: &str = "0.1.0";

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
type RemoteTunnel = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;

struct TestPaths {
  root: PathBuf,
  state_dir: PathBuf,
  rmux_socket: PathBuf,
  release_marker: PathBuf,
}

struct CapturedAttachment {
  stream: RemoteTunnel,
  resume_sequence: u64,
  shell_pid: String,
}

struct RemoteShellTest {
  paths: TestPaths,
  rmuxd: JoinHandle<Result<(), rmuxd::DaemonError>>,
  shutdown: watch::Sender<bool>,
  ctld: JoinHandle<Result<(), ctld::DaemonError>>,
  control_identity: ControlIdentity,
  host: ctl_core::HostConfig,
  rmux_identity: RmuxIdentity,
  session: SessionInfo,
}

impl TestPaths {
  fn new() -> Self {
    let suffix = Uuid::new_v4().simple().to_string();
    let root = PathBuf::from("/tmp").join(format!("ctld-remote-session-{}", &suffix[..12]));
    std::fs::create_dir(&root).expect("create private test directory");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
      .expect("make test directory owner-only");
    Self {
      state_dir: root.join("state"),
      rmux_socket: root.join("rmux.sock"),
      release_marker: root.join("release-delayed-output"),
      root,
    }
  }
}

impl Drop for TestPaths {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.root);
  }
}

/// The gateway is deliberately restarted while the local daemon and its PTY
/// stay alive.  A caller resumes by durable session ID and raw output
/// sequence, then regains the released input lease.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_shell_survives_ctld_restart_and_resumes_from_its_sequence() -> TestResult {
  let paths = TestPaths::new();
  let rmuxd = spawn_rmuxd(&paths.rmux_socket);
  wait_for_unix_socket(&paths.rmux_socket).await?;

  let device = ctld::initialize(&paths.state_dir)?;
  let first_listener = TcpListener::bind("127.0.0.1:0").await?;
  let endpoint = first_listener.local_addr()?;
  let invitation = ctld::create_pairing_invitation(
    &paths.state_dir,
    endpoint.to_string(),
    "integration-client".into(),
    expiration_after(Duration::from_mins(1))?,
  )?;
  assert_eq!(invitation.device_id, device.device_id);

  let config =
    ctld::DaemonConfig::with_defaults(paths.state_dir.clone(), paths.rmux_socket.clone());
  let (first_shutdown, first_ctld) = spawn_ctld(first_listener, config.clone());

  let control_identity = ControlIdentity::generate()?;
  let host = pair(
    &invitation,
    "test-host".into(),
    &control_identity,
    RMUX_CLIENT_NAME,
    RMUX_CLIENT_VERSION,
  )
  .await?;
  let rmux_identity = rmux_identity();

  let session = create_remote_shell(
    &host,
    &control_identity,
    &rmux_identity,
    &paths.release_marker,
  )
  .await?;

  let first_attachment =
    attach_and_capture_started(&host, &control_identity, &rmux_identity, &session).await?;

  // The shell cannot emit `delayed` until this test creates the marker.  Stop
  // the relay first so this is a direct test that `ctld` does not own the PTY.
  stop_ctld(first_shutdown, first_ctld).await?;
  drop(first_attachment.stream);
  std::fs::write(&paths.release_marker, b"release")?;

  let locally_running = wait_for_local_sequence(
    &paths.rmux_socket,
    &rmux_identity,
    &session.session_id,
    first_attachment.resume_sequence,
  )
  .await?;
  assert_eq!(locally_running.status, SessionStatus::Running);
  assert!(
    locally_running.next_sequence > first_attachment.resume_sequence,
    "rmuxd did not journal delayed output while ctld was stopped"
  );

  // Rebind exactly the same loopback endpoint so the saved host configuration
  // needs neither re-pairing nor a changed remote address.
  let second_listener = bind_same_loopback(endpoint).await?;
  let (second_shutdown, second_ctld) = spawn_ctld(second_listener, config);

  let second_tunnel = open_remote_tunnel(&host, &control_identity).await?;
  let (mut second_attachment, second_attached) = begin_attach(
    second_tunnel,
    &rmux_identity,
    AttachRequest {
      session: session.session_id.clone(),
      resume_from: Some(first_attachment.resume_sequence),
      terminal_size: TerminalSize::default(),
      request_input_lease: true,
      request_layout_lease: false,
      request_command_line: false,
    },
  )
  .await?;
  assert_eq!(
    second_attached.replay_from,
    first_attachment.resume_sequence
  );
  assert!(
    second_attached.input_lease.owned_by_client,
    "ctld shutdown should release the prior attachment's input lease"
  );

  let delayed_marker = format!("delayed:{}", first_attachment.shell_pid);
  let (delayed_output, delayed_sequence) =
    read_output_until(&mut second_attachment, delayed_marker.as_bytes()).await?;
  assert_eq!(
    pid_after(&delayed_output, "delayed:")?,
    first_attachment.shell_pid
  );
  assert!(delayed_sequence > first_attachment.resume_sequence);

  write_frame(
    &mut second_attachment,
    &ClientMessage::Input {
      data: b"finish\n".to_vec(),
    },
  )
  .await?;
  let final_marker = format!("final:{}:finish", first_attachment.shell_pid);
  let (final_output, _) =
    read_output_until(&mut second_attachment, final_marker.as_bytes()).await?;
  assert_eq!(
    pid_after(&final_output, "final:")?,
    first_attachment.shell_pid
  );
  wait_for_session_end(&mut second_attachment).await?;
  drop(second_attachment);

  stop_ctld(second_shutdown, second_ctld).await?;
  wait_for_rmuxd_exit(rmuxd).await
}

/// A client can reconnect while its former gateway relay is still physically
/// open but silent. `rmuxd` must expire that stale attachment, preserve the
/// shell, and let the new attachment acquire the released leases in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_reconnect_recovers_after_silent_gateway_relay_expiry() -> TestResult {
  let RemoteShellTest {
    paths,
    rmuxd,
    shutdown,
    ctld,
    control_identity,
    host,
    rmux_identity,
    session,
  } = start_remote_shell_test(Duration::from_secs(1)).await?;
  let CapturedAttachment {
    stream: mut stale_stream,
    resume_sequence,
    shell_pid,
  } = attach_and_capture_started(&host, &control_identity, &rmux_identity, &session).await?;

  // Claim layout without an initial resize. The stale stream now holds both
  // leases, but sends no heartbeat or other client traffic from this point.
  let stale_layout = acquire_lease(&mut stale_stream, LeaseKind::Layout).await?;
  assert!(stale_layout.owned_by_client);

  let recovery_tunnel = open_remote_tunnel(&host, &control_identity).await?;
  let (mut recovery_stream, recovery_attachment) = begin_attach(
    recovery_tunnel,
    &rmux_identity,
    AttachRequest {
      session: session.session_id.clone(),
      resume_from: Some(resume_sequence),
      terminal_size: TerminalSize::default(),
      request_input_lease: true,
      request_layout_lease: true,
      request_command_line: false,
    },
  )
  .await?;
  assert!(recovery_attachment.input_lease.held);
  assert!(!recovery_attachment.input_lease.owned_by_client);
  assert!(recovery_attachment.layout_lease.held);
  assert!(!recovery_attachment.layout_lease.owned_by_client);

  let mut heartbeat_nonce =
    recover_requested_leases(&mut recovery_stream, &recovery_attachment, true, true).await?;

  // Acquiring the layout lease does not resize by itself. The interactive
  // client performs this one explicit resize after its later grant; mirror it
  // here to prove a late stale resize cannot override that owner.
  let recovered_size = terminal_size(111, 33);
  write_frame(
    &mut recovery_stream,
    &ClientMessage::Resize {
      terminal_size: recovered_size.clone(),
    },
  )
  .await?;
  wait_for_remote_terminal_size(
    &host,
    &control_identity,
    &rmux_identity,
    &session.session_id,
    &recovered_size,
  )
  .await?;
  send_heartbeat(&mut recovery_stream, &mut heartbeat_nonce).await?;

  assert_stale_request_rejected(
    &mut stale_stream,
    ClientMessage::Input {
      data: b"stale\n".to_vec(),
    },
    ErrorCode::InputLeaseRequired,
  )
  .await?;
  send_heartbeat(&mut recovery_stream, &mut heartbeat_nonce).await?;
  assert_stale_request_rejected(
    &mut stale_stream,
    ClientMessage::Resize {
      terminal_size: terminal_size(13, 7),
    },
    ErrorCode::LayoutLeaseRequired,
  )
  .await?;
  send_heartbeat(&mut recovery_stream, &mut heartbeat_nonce).await?;
  let current = remote_session_info(
    &host,
    &control_identity,
    &rmux_identity,
    &session.session_id,
  )
  .await?;
  assert_eq!(current.terminal_size, recovered_size);

  std::fs::write(&paths.release_marker, b"release")?;
  let delayed_marker = format!("delayed:{shell_pid}");
  let (delayed_output, delayed_sequence) =
    read_output_until(&mut recovery_stream, delayed_marker.as_bytes()).await?;
  assert_eq!(pid_after(&delayed_output, "delayed:")?, shell_pid);
  assert!(delayed_sequence > resume_sequence);

  write_frame(
    &mut recovery_stream,
    &ClientMessage::Input {
      data: b"finish\n".to_vec(),
    },
  )
  .await?;
  let final_marker = format!("final:{shell_pid}:finish");
  let (final_output, _) = read_output_until(&mut recovery_stream, final_marker.as_bytes()).await?;
  assert_eq!(pid_after(&final_output, "final:")?, shell_pid);
  wait_for_session_end(&mut recovery_stream).await?;
  drop((recovery_stream, stale_stream));

  stop_ctld(shutdown, ctld).await?;
  wait_for_rmuxd_exit(rmuxd).await
}

async fn start_remote_shell_test(
  attachment_liveness_timeout: Duration,
) -> TestResult<RemoteShellTest> {
  let paths = TestPaths::new();
  let rmuxd = spawn_rmuxd_with_liveness(&paths.rmux_socket, attachment_liveness_timeout);
  wait_for_unix_socket(&paths.rmux_socket).await?;

  ctld::initialize(&paths.state_dir)?;
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  let endpoint = listener.local_addr()?;
  let invitation = ctld::create_pairing_invitation(
    &paths.state_dir,
    endpoint.to_string(),
    "integration-client".into(),
    expiration_after(Duration::from_mins(1))?,
  )?;
  let config =
    ctld::DaemonConfig::with_defaults(paths.state_dir.clone(), paths.rmux_socket.clone());
  let (shutdown, ctld) = spawn_ctld(listener, config);

  let control_identity = ControlIdentity::generate()?;
  let host = pair(
    &invitation,
    "test-host".into(),
    &control_identity,
    RMUX_CLIENT_NAME,
    RMUX_CLIENT_VERSION,
  )
  .await?;
  let rmux_identity = rmux_identity();
  let session = create_remote_shell(
    &host,
    &control_identity,
    &rmux_identity,
    &paths.release_marker,
  )
  .await?;

  Ok(RemoteShellTest {
    paths,
    rmuxd,
    shutdown,
    ctld,
    control_identity,
    host,
    rmux_identity,
    session,
  })
}

fn spawn_rmuxd(socket_path: &Path) -> JoinHandle<Result<(), rmuxd::DaemonError>> {
  spawn_rmuxd_with_liveness(socket_path, Duration::from_secs(30))
}

fn spawn_rmuxd_with_liveness(
  socket_path: &Path,
  attachment_liveness_timeout: Duration,
) -> JoinHandle<Result<(), rmuxd::DaemonError>> {
  let socket_path = socket_path.to_path_buf();
  tokio::spawn(async move {
    run_rmuxd(RmuxDaemonConfig {
      socket_path,
      journal_capacity_bytes: 64 * 1024,
      checkpoint_interval_bytes: 4 * 1024,
      startup_idle_timeout: Duration::from_secs(10),
      attachment_liveness_timeout,
    })
    .await
  })
}

fn spawn_ctld(
  listener: TcpListener,
  config: ctld::DaemonConfig,
) -> (
  watch::Sender<bool>,
  JoinHandle<Result<(), ctld::DaemonError>>,
) {
  let (shutdown, receiver) = watch::channel(false);
  let task = tokio::spawn(async move { ctld::serve(listener, config, receiver).await });
  (shutdown, task)
}

async fn stop_ctld(
  shutdown: watch::Sender<bool>,
  task: JoinHandle<Result<(), ctld::DaemonError>>,
) -> TestResult {
  shutdown.send(true)?;
  timeout(TEST_TIMEOUT, task)
    .await
    .map_err(|_| "ctld did not stop after shutdown")???;
  Ok(())
}

async fn create_remote_shell(
  host: &ctl_core::HostConfig,
  control_identity: &ControlIdentity,
  rmux_identity: &RmuxIdentity,
  release_marker: &Path,
) -> TestResult<SessionInfo> {
  let script = concat!(
    "printf 'started:%s\\n' \"$$\"; ",
    "while [ ! -f \"$1\" ]; do sleep 0.01; done; ",
    "printf 'delayed:%s\\n' \"$$\"; ",
    "IFS= read -r line; ",
    "printf 'final:%s:%s\\n' \"$$\" \"$line\""
  );
  let response = rmux_request(
    open_remote_tunnel(host, control_identity).await?,
    rmux_identity,
    ClientMessage::CreateSession {
      name: Some("remote-persistent".into()),
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec![
          "-c".into(),
          script.into(),
          "remote-persistent".into(),
          release_marker.to_string_lossy().into_owned(),
        ],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  let ServerMessage::SessionCreated { session } = response else {
    return Err(format!("expected remote session_created, received {response:?}").into());
  };
  Ok(session)
}

async fn attach_and_capture_started(
  host: &ctl_core::HostConfig,
  control_identity: &ControlIdentity,
  rmux_identity: &RmuxIdentity,
  session: &SessionInfo,
) -> TestResult<CapturedAttachment> {
  let tunnel = open_remote_tunnel(host, control_identity).await?;
  let (mut stream, attached) = begin_attach(
    tunnel,
    rmux_identity,
    AttachRequest {
      session: session.session_id.clone(),
      resume_from: None,
      terminal_size: TerminalSize::default(),
      request_input_lease: true,
      request_layout_lease: false,
      request_command_line: false,
    },
  )
  .await?;
  assert!(
    attached.input_lease.owned_by_client,
    "the first remote attachment should own input"
  );
  let (started_output, resume_sequence) = read_output_until(&mut stream, b"started:").await?;
  assert!(
    resume_sequence > 0,
    "started output must advance the stream"
  );
  Ok(CapturedAttachment {
    stream,
    resume_sequence,
    shell_pid: pid_after(&started_output, "started:")?,
  })
}

async fn acquire_lease<S>(stream: &mut S, lease: LeaseKind) -> TestResult<LeaseStatus>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  write_frame(stream, &ClientMessage::AcquireLease { lease }).await?;
  loop {
    match required_rmux_message(stream).await? {
      ServerMessage::LeaseStatus {
        lease: actual,
        status,
      } if actual == lease => return Ok(status),
      ServerMessage::HeartbeatAck { .. }
      | ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. } => {}
      message => return Err(format!("expected lease status, received {message:?}").into()),
    }
  }
}

async fn recover_requested_leases(
  stream: &mut RemoteTunnel,
  attached: &AttachedSession,
  request_input_lease: bool,
  request_layout_lease: bool,
) -> TestResult<u64> {
  let deadline = Instant::now() + TEST_TIMEOUT;
  let mut input = attached.input_lease.clone();
  let mut layout = attached.layout_lease.clone();
  let mut heartbeat_nonce = 0_u64;

  loop {
    heartbeat_nonce = heartbeat_nonce.wrapping_add(1);
    write_frame(
      stream,
      &ClientMessage::Heartbeat {
        nonce: heartbeat_nonce,
      },
    )
    .await?;
    if request_input_lease && !input.owned_by_client {
      write_frame(
        stream,
        &ClientMessage::AcquireLease {
          lease: LeaseKind::Input,
        },
      )
      .await?;
    }
    if request_layout_lease && !layout.owned_by_client {
      write_frame(
        stream,
        &ClientMessage::AcquireLease {
          lease: LeaseKind::Layout,
        },
      )
      .await?;
    }

    let mut heartbeat_acknowledged = false;
    let mut input_status_received = !request_input_lease || input.owned_by_client;
    let mut layout_status_received = !request_layout_lease || layout.owned_by_client;
    while !heartbeat_acknowledged || !input_status_received || !layout_status_received {
      match required_rmux_message(stream).await? {
        ServerMessage::HeartbeatAck { nonce } if nonce == heartbeat_nonce => {
          heartbeat_acknowledged = true;
        }
        ServerMessage::LeaseStatus {
          lease: LeaseKind::Input,
          status,
        } if request_input_lease => {
          input = status;
          input_status_received = true;
        }
        ServerMessage::LeaseStatus {
          lease: LeaseKind::Layout,
          status,
        } if request_layout_lease => {
          layout = status;
          layout_status_received = true;
        }
        ServerMessage::Output { .. } | ServerMessage::Checkpoint { .. } => {}
        message => {
          return Err(
            format!("expected heartbeat or requested lease status, received {message:?}").into(),
          );
        }
      }
    }

    if (!request_input_lease || input.owned_by_client)
      && (!request_layout_lease || layout.owned_by_client)
    {
      return Ok(heartbeat_nonce);
    }
    if Instant::now() >= deadline {
      return Err("stale gateway relay did not release its requested leases".into());
    }
    sleep(attached.liveness.heartbeat_interval).await;
  }
}

async fn send_heartbeat(stream: &mut RemoteTunnel, nonce: &mut u64) -> TestResult {
  *nonce = nonce.wrapping_add(1);
  write_frame(stream, &ClientMessage::Heartbeat { nonce: *nonce }).await?;
  loop {
    match required_rmux_message(stream).await? {
      ServerMessage::HeartbeatAck { nonce: actual } if actual == *nonce => return Ok(()),
      ServerMessage::Output { .. } | ServerMessage::Checkpoint { .. } => {}
      message => {
        return Err(format!("expected heartbeat acknowledgement, received {message:?}").into());
      }
    }
  }
}

async fn assert_stale_request_rejected<S>(
  stream: &mut S,
  message: ClientMessage,
  expected_error: ErrorCode,
) -> TestResult
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  match write_frame(stream, &message).await {
    Ok(()) => {}
    Err(CodecError::Io(_)) => return Ok(()),
    Err(error) => return Err(error.into()),
  }

  match timeout(
    Duration::from_millis(500),
    read_frame::<_, ServerMessage>(stream),
  )
  .await
  {
    Ok(Ok(None) | Err(CodecError::Io(_))) => Ok(()),
    Ok(Ok(Some(ServerMessage::Error { code, .. }))) if code == expected_error => Ok(()),
    Ok(Ok(Some(message))) => Err(format!("stale attachment received {message:?}").into()),
    Ok(Err(error)) => Err(error.into()),
    Err(_) => Err("stale attachment was neither rejected nor closed".into()),
  }
}

async fn open_remote_tunnel(
  host: &ctl_core::HostConfig,
  identity: &ControlIdentity,
) -> TestResult<RemoteTunnel> {
  Ok(open_rmux_tunnel(host, identity, RMUX_CLIENT_NAME, RMUX_CLIENT_VERSION).await?)
}

async fn remote_session_info(
  host: &ctl_core::HostConfig,
  control_identity: &ControlIdentity,
  rmux_identity: &RmuxIdentity,
  session_id: &str,
) -> TestResult<SessionInfo> {
  let response = rmux_request(
    open_remote_tunnel(host, control_identity).await?,
    rmux_identity,
    ClientMessage::ListSessions,
  )
  .await?;
  let ServerMessage::SessionList { sessions } = response else {
    return Err(format!("expected session list, received {response:?}").into());
  };
  sessions
    .into_iter()
    .find(|session| session.session_id == session_id)
    .ok_or_else(|| format!("session '{session_id}' disappeared").into())
}

async fn wait_for_remote_terminal_size(
  host: &ctl_core::HostConfig,
  control_identity: &ControlIdentity,
  rmux_identity: &RmuxIdentity,
  session_id: &str,
  expected: &TerminalSize,
) -> TestResult {
  let deadline = Instant::now() + TEST_TIMEOUT;
  loop {
    let session = remote_session_info(host, control_identity, rmux_identity, session_id).await?;
    if session.terminal_size == *expected {
      return Ok(());
    }
    if Instant::now() >= deadline {
      return Err(
        format!(
          "session '{session_id}' did not resize to {}x{}",
          expected.columns, expected.rows
        )
        .into(),
      );
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn wait_for_local_sequence(
  socket_path: &Path,
  rmux_identity: &RmuxIdentity,
  session_id: &str,
  after_sequence: u64,
) -> TestResult<SessionInfo> {
  let deadline = Instant::now() + TEST_TIMEOUT;
  loop {
    let local_stream = UnixStream::connect(socket_path).await?;
    let response = rmux_request(local_stream, rmux_identity, ClientMessage::ListSessions).await?;
    let ServerMessage::SessionList { sessions } = response else {
      return Err(format!("expected local session list, received {response:?}").into());
    };
    let session = sessions
      .into_iter()
      .find(|candidate| candidate.session_id == session_id)
      .ok_or_else(|| format!("session '{session_id}' disappeared from local rmuxd"))?;
    if session.status == SessionStatus::Running && session.next_sequence > after_sequence {
      return Ok(session);
    }
    if Instant::now() >= deadline {
      return Err(format!(
        "session '{session_id}' did not remain running with output after sequence {after_sequence}"
      )
      .into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn bind_same_loopback(address: SocketAddr) -> TestResult<TcpListener> {
  let deadline = Instant::now() + TEST_TIMEOUT;
  loop {
    match TcpListener::bind(address).await {
      Ok(listener) => return Ok(listener),
      Err(_error) if Instant::now() < deadline => sleep(Duration::from_millis(10)).await,
      Err(error) => return Err(error.into()),
    }
  }
}

async fn wait_for_unix_socket(socket_path: &Path) -> TestResult {
  let deadline = Instant::now() + TEST_TIMEOUT;
  loop {
    match UnixStream::connect(socket_path).await {
      Ok(stream) => {
        drop(stream);
        return Ok(());
      }
      Err(_error) if Instant::now() < deadline => sleep(Duration::from_millis(10)).await,
      Err(error) => return Err(error.into()),
    }
  }
}

async fn read_output_until<S>(stream: &mut S, expected: &[u8]) -> TestResult<(Vec<u8>, u64)>
where
  S: AsyncRead + Unpin,
{
  let mut output = Vec::new();
  loop {
    match required_rmux_message(stream).await? {
      ServerMessage::Output {
        sequence_end, data, ..
      } => {
        output.extend(data);
        if contains_bytes(&output, expected) {
          return Ok((output, sequence_end));
        }
      }
      ServerMessage::Checkpoint { .. }
      | ServerMessage::HeartbeatAck { .. }
      | ServerMessage::LeaseStatus { .. } => {}
      message => return Err(format!("expected output, received {message:?}").into()),
    }
  }
}

async fn wait_for_session_end<S>(stream: &mut S) -> TestResult
where
  S: AsyncRead + Unpin,
{
  loop {
    match required_rmux_message(stream).await? {
      ServerMessage::Output { .. }
      | ServerMessage::Checkpoint { .. }
      | ServerMessage::HeartbeatAck { .. }
      | ServerMessage::LeaseStatus { .. } => {}
      ServerMessage::SessionEnded { .. } => return Ok(()),
      message => {
        return Err(format!("expected output or session end, received {message:?}").into());
      }
    }
  }
}

async fn required_rmux_message<S>(stream: &mut S) -> TestResult<ServerMessage>
where
  S: AsyncRead + Unpin,
{
  timeout(TEST_TIMEOUT, read_frame(stream))
    .await
    .map_err(|_| "timed out waiting for rmuxd")??
    .ok_or_else(|| "rmuxd closed the attachment unexpectedly".into())
}

async fn wait_for_rmuxd_exit(daemon: JoinHandle<Result<(), rmuxd::DaemonError>>) -> TestResult {
  timeout(TEST_TIMEOUT, daemon)
    .await
    .map_err(|_| "rmuxd did not exit after the final remote shell ended")???;
  Ok(())
}

fn rmux_identity() -> RmuxIdentity {
  RmuxIdentity {
    name: RMUX_CLIENT_NAME.into(),
    version: RMUX_CLIENT_VERSION.into(),
  }
}

fn expiration_after(duration: Duration) -> TestResult<u64> {
  let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
  Ok(u64::try_from((now + duration).as_millis())?)
}

fn pid_after(output: &[u8], prefix: &str) -> TestResult<String> {
  let output = String::from_utf8_lossy(output);
  let offset = output
    .find(prefix)
    .ok_or_else(|| format!("output did not contain '{prefix}': {output:?}"))?
    + prefix.len();
  let pid = output[offset..]
    .chars()
    .take_while(char::is_ascii_digit)
    .collect::<String>();
  if pid.is_empty() {
    return Err(format!("output did not contain a PID after '{prefix}': {output:?}").into());
  }
  Ok(pid)
}

fn terminal_size(columns: u16, rows: u16) -> TerminalSize {
  TerminalSize {
    columns,
    rows,
    pixel_width: 0,
    pixel_height: 0,
  }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
  haystack
    .windows(needle.len())
    .any(|candidate| candidate == needle)
}
