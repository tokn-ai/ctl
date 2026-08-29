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
  let daemon_socket = socket_path.clone();
  let daemon = tokio::spawn(async move {
    run(DaemonConfig {
      socket_path: daemon_socket,
      journal_capacity_bytes: 64 * 1024,
      startup_idle_timeout: Duration::from_secs(5),
    })
    .await
  });

  let mut create = connect_when_ready(&socket_path).await?;
  handshake(&mut create).await?;
  write_frame(
    &mut create,
    &ClientMessage::CreateSession {
      name: Some("persistent".into()),
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec![
          "-c".into(),
          "printf 'before\\n'; IFS= read -r line; printf 'after:%s\\n' \"$line\"".into(),
        ],
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
  drop(create);

  let mut first_attach = connect_when_ready(&socket_path).await?;
  handshake(&mut first_attach).await?;
  write_frame(
    &mut first_attach,
    &ClientMessage::AttachSession {
      session: session.session_id.clone(),
      resume_from: None,
    },
  )
  .await?;
  assert!(matches!(
    required_message(&mut first_attach).await?,
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

  let mut second_attach = connect_when_ready(&socket_path).await?;
  handshake(&mut second_attach).await?;
  write_frame(
    &mut second_attach,
    &ClientMessage::AttachSession {
      session: session.session_id,
      resume_from: Some(resume_sequence),
    },
  )
  .await?;
  assert!(matches!(
    required_message(&mut second_attach).await?,
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
