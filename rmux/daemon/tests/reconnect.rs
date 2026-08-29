#![cfg(unix)]

use rmux_proto::{
  ClientMessage, CommandSpec, PROTOCOL_VERSION, ServerMessage, TerminalSize, read_frame,
  write_frame,
};
use rmuxd::{DaemonConfig, run};
use std::error::Error;
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::time::{Instant, sleep, timeout};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_survives_client_disconnect_and_resumes_from_sequence() -> TestResult {
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 64 * 1024, 4 * 1024);
  let session = create_shell_session(
    &socket_path,
    "persistent",
    "printf 'before\\n'; IFS= read -r line; printf 'after:%s\\n' \"$line\"",
  )
  .await?;

  let (mut first_attach, first_attached) =
    attach_session(&socket_path, &session.session_id, None).await?;
  assert!(matches!(
    first_attached,
    ServerMessage::Attached {
      replay_from: 0,
      history_gap: false,
      ..
    }
  ));
  let (first_output, resume_sequence) = read_output_until(&mut first_attach, b"before").await?;
  assert!(contains_bytes(&first_output, b"before"));
  write_frame(&mut first_attach, &ClientMessage::Detach).await?;
  drop(first_attach);

  let (mut second_attach, second_attached) =
    attach_session(&socket_path, &session.session_id, Some(resume_sequence)).await?;
  assert!(matches!(
    second_attached,
    ServerMessage::Attached {
      replay_from,
      history_gap: false,
      ..
    } if replay_from == resume_sequence
  ));

  write_frame(
    &mut second_attach,
    &ClientMessage::Input {
      data: b"go\n".to_vec(),
    },
  )
  .await?;
  let (second_output, _) = read_output_until(&mut second_attach, b"after:go").await?;
  assert!(!contains_bytes(&second_output, b"before"));
  assert!(contains_bytes(&second_output, b"after:go"));

  loop {
    if matches!(
      required_message(&mut second_attach).await?,
      ServerMessage::SessionEnded { .. }
    ) {
      break;
    }
  }
  drop(second_attach);

  let daemon_result = timeout(Duration::from_secs(3), daemon)
    .await
    .map_err(|_| "rmuxd did not exit after its final session ended")?;
  daemon_result??;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_restores_terminal_state_after_journal_compaction() -> TestResult {
  let test_directory = TestDirectory::new();
  let socket_path = test_directory.path.join("rmux.sock");
  let daemon = spawn_daemon(&socket_path, 32, 1);
  let session = create_shell_session(
    &socket_path,
    "checkpoint",
    "printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; printf '\\033[2J\\033[Hcheckpoint-ready'; IFS= read -r line",
  )
  .await?;

  let (mut first_attach, first_attached) =
    attach_session(&socket_path, &session.session_id, Some(0)).await?;
  assert!(matches!(first_attached, ServerMessage::Attached { .. }));
  let (initial_output, _) = read_output_until(&mut first_attach, b"checkpoint-ready").await?;
  assert!(contains_bytes(&initial_output, b"checkpoint-ready"));
  write_frame(&mut first_attach, &ClientMessage::Detach).await?;
  drop(first_attach);

  let (mut restored_attach, attached) =
    attach_session(&socket_path, &session.session_id, Some(0)).await?;
  let ServerMessage::Attached {
    checkpoint: Some(checkpoint),
    history_gap: true,
    terminal_size_mismatch: false,
    ..
  } = attached
  else {
    return Err(format!("expected checkpoint-backed attach, received {attached:?}").into());
  };
  assert!(checkpoint.is_supported());

  let mut restored_terminal = avt::Vt::new(80, 24);
  restored_terminal.feed_str(&String::from_utf8(checkpoint.payload)?);
  assert!(
    restored_terminal
      .text()
      .join("\n")
      .contains("checkpoint-ready")
  );

  write_frame(
    &mut restored_attach,
    &ClientMessage::Input {
      data: b"go\n".to_vec(),
    },
  )
  .await?;
  loop {
    if matches!(
      required_message(&mut restored_attach).await?,
      ServerMessage::SessionEnded { .. }
    ) {
      break;
    }
  }
  drop(restored_attach);

  let daemon_result = timeout(Duration::from_secs(3), daemon)
    .await
    .map_err(|_| "rmuxd did not exit after checkpoint test")?;
  daemon_result??;
  Ok(())
}

fn spawn_daemon(
  socket_path: &Path,
  journal_capacity_bytes: usize,
  checkpoint_interval_bytes: usize,
) -> tokio::task::JoinHandle<Result<(), rmuxd::DaemonError>> {
  let socket_path = socket_path.to_path_buf();
  tokio::spawn(async move {
    run(DaemonConfig {
      socket_path,
      journal_capacity_bytes,
      checkpoint_interval_bytes,
      startup_idle_timeout: Duration::from_secs(5),
    })
    .await
  })
}

async fn create_shell_session(
  socket_path: &Path,
  name: &str,
  script: &str,
) -> TestResult<rmux_proto::SessionInfo> {
  let mut create = connect_when_ready(socket_path).await?;
  handshake(&mut create).await?;
  write_frame(
    &mut create,
    &ClientMessage::CreateSession {
      name: Some(name.into()),
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), script.into()],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  let created = required_message(&mut create).await?;
  let ServerMessage::SessionCreated { session } = created else {
    return Err(format!("expected session_created, received {created:?}").into());
  };
  Ok(session)
}

async fn attach_session(
  socket_path: &Path,
  session: &str,
  resume_from: Option<u64>,
) -> TestResult<(UnixStream, ServerMessage)> {
  let mut stream = connect_when_ready(socket_path).await?;
  handshake(&mut stream).await?;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: session.into(),
      resume_from,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  let attached = required_message(&mut stream).await?;
  Ok((stream, attached))
}

async fn connect_when_ready(socket_path: &Path) -> TestResult<UnixStream> {
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    match UnixStream::connect(socket_path).await {
      Ok(stream) => return Ok(stream),
      Err(_) if Instant::now() < deadline => {
        sleep(Duration::from_millis(10)).await;
      }
      Err(error) => return Err(error.into()),
    }
  }
}

async fn handshake(stream: &mut UnixStream) -> TestResult {
  write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: "integration-test".into(),
      client_version: env!("CARGO_PKG_VERSION").into(),
    },
  )
  .await?;
  assert!(matches!(
    required_message(stream).await?,
    ServerMessage::HandshakeAccepted {
      protocol_version: PROTOCOL_VERSION,
      ..
    }
  ));
  Ok(())
}

async fn required_message(stream: &mut UnixStream) -> TestResult<ServerMessage> {
  timeout(Duration::from_secs(3), read_frame(stream))
    .await
    .map_err(|_| "timed out waiting for rmuxd")??
    .ok_or_else(|| "rmuxd closed the connection unexpectedly".into())
}

async fn read_output_until(stream: &mut UnixStream, expected: &[u8]) -> TestResult<(Vec<u8>, u64)> {
  let mut output = Vec::new();
  loop {
    match required_message(stream).await? {
      ServerMessage::Output {
        sequence_end, data, ..
      } => {
        output.extend(data);
        if contains_bytes(&output, expected) {
          return Ok((output, sequence_end));
        }
      }
      message => return Err(format!("expected output, received {message:?}").into()),
    }
  }
}

struct TestDirectory {
  path: std::path::PathBuf,
}

impl TestDirectory {
  fn new() -> Self {
    Self {
      path: std::path::PathBuf::from("/tmp").join(format!(
        "rmux-t-{}",
        &Uuid::new_v4().simple().to_string()[..8]
      )),
    }
  }
}

impl Drop for TestDirectory {
  fn drop(&mut self) {
    let _ignored = std::fs::remove_dir_all(&self.path);
  }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
  haystack
    .windows(needle.len())
    .any(|candidate| candidate == needle)
}
