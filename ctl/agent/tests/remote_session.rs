#![cfg(unix)]

use rmux_client::{
  AttachRequest, ClientIdentity, DEFAULT_PRESENTATION_WINDOW_BYTES, begin_attach, request,
  resume_attach,
};
use rmux_proto::{
  ClientMessage, CommandSpec, ServerMessage, SessionStatus, TerminalSize, read_frame, write_frame,
};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, DuplexStream};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};
use uuid::Uuid;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

struct TestDirectory {
  path: PathBuf,
}

impl TestDirectory {
  fn new() -> Self {
    let suffix = Uuid::new_v4().simple().to_string();
    let path = PathBuf::from("/tmp").join(format!("ctl-agent-remote-{}", &suffix[..12]));
    std::fs::create_dir(&path).expect("create test directory");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
      .expect("make test directory private");
    Self { path }
  }
}

impl Drop for TestDirectory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.path);
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_scoped_gateway_disconnect_does_not_end_the_remote_session() -> TestResult {
  let directory = TestDirectory::new();
  let socket = directory.path.join("rmux.sock");
  let daemon = spawn_rmuxd(&socket);
  wait_for_socket(&socket).await?;
  let identity = ClientIdentity {
    name: "ctl-agent-integration-test".into(),
    version: "0.1.0".into(),
  };

  let created = request(
    open_gateway(&socket).await?,
    &identity,
    ClientMessage::CreateSession {
      name: Some("remote".into()),
      command: Some(CommandSpec {
        program: "sh".into(),
        arguments: vec![
          "-c".into(),
          "printf 'ready\\n'; IFS= read -r line; printf 'received:%s\\n' \"$line\"".into(),
        ],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  let ServerMessage::SessionCreated { session } = created else {
    return Err(format!("expected session_created, received {created:?}").into());
  };

  let attach_request = AttachRequest {
    session: session.session_id.clone(),
    resume_from: None,
    terminal_size: TerminalSize::default(),
    request_input_lease: true,
    request_layout_lease: false,
    request_command_line: false,
    request_running_command: false,
    presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
  };
  let (mut attachment, attached) = begin_attach(
    open_gateway(&socket).await?,
    &identity,
    attach_request.clone(),
  )
  .await?;
  if let Some(checkpoint) = &attached.checkpoint {
    write_frame(
      &mut attachment,
      &ClientMessage::PresentationApplied {
        sequence: checkpoint.sequence,
      },
    )
    .await?;
  }
  read_output_until(&mut attachment, b"ready").await?;

  // This is the lifecycle of a broken SSH channel: its disposable `ctl-agent`
  // relay disappears, but the daemon-owned shell must remain alive.
  drop(attachment);
  sleep(Duration::from_millis(50)).await;

  let listed = request(
    open_gateway(&socket).await?,
    &identity,
    ClientMessage::ListSessions,
  )
  .await?;
  let ServerMessage::SessionList { sessions } = listed else {
    return Err(format!("expected session_list, received {listed:?}").into());
  };
  assert_eq!(sessions.len(), 1);
  assert_eq!(sessions[0].session_id, session.session_id);
  assert_eq!(sessions[0].status, SessionStatus::Running);

  let (mut resumed, resumed_attachment) = resume_attach(
    open_gateway(&socket).await?,
    &identity,
    attached.attachment_token,
    attach_request,
  )
  .await?;
  assert!(resumed_attachment.input_lease.owned_by_client);
  if let Some(checkpoint) = &resumed_attachment.checkpoint {
    write_frame(
      &mut resumed,
      &ClientMessage::PresentationApplied {
        sequence: checkpoint.sequence,
      },
    )
    .await?;
  }
  write_frame(
    &mut resumed,
    &ClientMessage::Input {
      data: b"through-reconnect\n".to_vec(),
    },
  )
  .await?;
  read_output_until(&mut resumed, b"received:through-reconnect").await?;
  wait_for_session_end(&mut resumed).await?;
  drop(resumed);

  timeout(TEST_TIMEOUT, daemon)
    .await
    .map_err(|_| "rmuxd did not exit after its final session ended")???;
  Ok(())
}

async fn wait_for_session_end(stream: &mut DuplexStream) -> TestResult {
  loop {
    let message = timeout(TEST_TIMEOUT, read_frame::<_, ServerMessage>(stream))
      .await
      .map_err(|_| "timed out waiting for session end")??
      .ok_or("attachment closed before session end")?;
    if matches!(message, ServerMessage::SessionEnded { .. }) {
      return Ok(());
    }
  }
}

async fn open_gateway(socket: &Path) -> TestResult<DuplexStream> {
  let config = ctl_agent::ConnectConfig::new(socket.into());
  let (client, gateway) = tokio::io::duplex(1024 * 1024);
  let (reader, writer) = tokio::io::split(gateway);
  tokio::spawn(async move {
    if let Err(error) = ctl_agent::connect(reader, writer, &config).await {
      eprintln!("test gateway failed: {error}");
    }
  });
  let mut client = client;
  let mut preface = vec![0_u8; ctl_agent::SSH_TRANSPORT_PREFACE.len()];
  client.read_exact(&mut preface).await?;
  if preface != ctl_agent::SSH_TRANSPORT_PREFACE {
    return Err("gateway returned an invalid transport preface".into());
  }
  Ok(client)
}

fn spawn_rmuxd(socket: &Path) -> JoinHandle<Result<(), rmuxd::DaemonError>> {
  let config = rmuxd::DaemonConfig {
    socket_path: socket.into(),
    startup_idle_timeout: TEST_TIMEOUT,
    ..rmuxd::DaemonConfig::default()
  };
  tokio::spawn(rmuxd::run(config))
}

async fn wait_for_socket(socket: &Path) -> TestResult {
  let deadline = Instant::now() + TEST_TIMEOUT;
  loop {
    match UnixStream::connect(socket).await {
      Ok(stream) => {
        drop(stream);
        return Ok(());
      }
      Err(error) if Instant::now() < deadline => {
        let _ = error;
        sleep(Duration::from_millis(10)).await;
      }
      Err(error) => return Err(error.into()),
    }
  }
}

async fn read_output_until(stream: &mut DuplexStream, marker: &[u8]) -> TestResult {
  let deadline = Instant::now() + TEST_TIMEOUT;
  let mut output = Vec::new();
  loop {
    let message = timeout(
      deadline.saturating_duration_since(Instant::now()),
      read_frame::<_, ServerMessage>(stream),
    )
    .await
    .map_err(|_| "timed out waiting for terminal output")??
    .ok_or("attachment closed before terminal output arrived")?;
    if let ServerMessage::Output { data, .. } = message {
      output.extend_from_slice(&data);
      if output.windows(marker.len()).any(|window| window == marker) {
        return Ok(());
      }
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_restart_ends_sessions_and_starts_the_installed_companion() -> TestResult {
  let directory = TestDirectory::new();
  let socket = directory.path.join("rmux.sock");
  let daemon = spawn_rmuxd(&socket);
  wait_for_socket(&socket).await?;
  let identity = ClientIdentity {
    name: "restart-test".into(),
    version: "0.1.0".into(),
  };
  let created = request(
    open_gateway(&socket).await?,
    &identity,
    ClientMessage::CreateSession {
      name: Some("restart-me".into()),
      command: None,
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  assert!(matches!(created, ServerMessage::SessionCreated { .. }));

  // The fixture executable records startup; an in-process daemon supplies the
  // replacement endpoint without depending on a separately built rmuxd binary.
  let marker = directory.path.join("started");
  let executable = directory.path.join("rmuxd");
  std::fs::write(
    &executable,
    format!(
      "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
      marker.display()
    ),
  )?;
  std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
  let mut config = ctl_agent::ConnectConfig::new(socket.clone());
  config.rmuxd_bin = Some(executable);

  let rejected = ctl_agent::restart::restart_rmux(&config, "expected", "different").await;
  assert!(rejected.is_err());
  assert!(!daemon.is_finished());
  assert!(!marker.exists());

  let replacement_socket = socket.clone();
  let replacement_marker = marker.clone();
  let replacement = tokio::spawn(async move {
    timeout(TEST_TIMEOUT, async {
      while !replacement_marker.exists() {
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .expect("installed companion started");
    spawn_rmuxd(&replacement_socket)
      .await
      .expect("replacement daemon task")
  });
  let outcome = timeout(
    TEST_TIMEOUT,
    ctl_agent::restart::restart_rmux(&config, "expected", "expected"),
  )
  .await??;
  assert_eq!(outcome.terminated_sessions, 1);
  timeout(TEST_TIMEOUT, daemon).await???;
  let arguments = std::fs::read_to_string(&marker)?;
  assert!(arguments.contains(socket.to_str().unwrap()));
  assert!(arguments.contains("--detach-from-terminal"));
  let sessions = request(
    open_gateway(&socket).await?,
    &identity,
    ClientMessage::ListSessions,
  )
  .await?;
  assert!(matches!(sessions, ServerMessage::SessionList { sessions } if sessions.is_empty()));
  let control = rmux_ipc::control_socket_path(&socket)?;
  rmux_ipc::request_local_daemon_restart(UnixStream::connect(control).await?).await?;
  timeout(TEST_TIMEOUT, replacement).await???;
  Ok(())
}

#[tokio::test]
async fn unsupported_restart_does_not_touch_a_live_data_endpoint() -> TestResult {
  let directory = TestDirectory::new();
  let socket = directory.path.join("rmux.sock");
  let listener = tokio::net::UnixListener::bind(&socket)?;
  let control = rmux_ipc::control_socket_path(&socket)?;
  let control_listener = tokio::net::UnixListener::bind(&control)?;
  let server = tokio::spawn(async move {
    let (mut stream, _) = control_listener.accept().await.unwrap();
    let hello: Option<rmux_ipc::LocalControlClientMessage> =
      rmux_ipc::read_local_control_frame(&mut stream)
        .await
        .unwrap();
    assert!(matches!(
      hello,
      Some(rmux_ipc::LocalControlClientMessage::Handshake { .. })
    ));
    rmux_ipc::write_local_control_frame(
      &mut stream,
      &rmux_ipc::LocalControlServerMessage::HandshakeAccepted {
        protocol_version: rmux_ipc::LOCAL_CONTROL_PROTOCOL_VERSION,
        restart_supported: false,
        managed_sessions_supported: false,
      },
    )
    .await
    .unwrap();
    let request: Option<rmux_ipc::LocalControlClientMessage> =
      rmux_ipc::read_local_control_frame(&mut stream)
        .await
        .unwrap();
    assert!(
      request.is_none(),
      "unsupported daemon must not receive restart"
    );
  });
  let mut config = ctl_agent::ConnectConfig::new(socket.clone());
  config.rmuxd_bin = Some("/bin/true".into());
  assert!(
    ctl_agent::restart::restart_rmux(&config, "expected", "expected")
      .await
      .is_err()
  );
  timeout(TEST_TIMEOUT, server).await??;
  assert!(socket.exists());
  drop(UnixStream::connect(&socket).await?);
  drop(listener);
  Ok(())
}
