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
