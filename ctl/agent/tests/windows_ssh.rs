#![cfg(windows)]

use ctl_core::SshTransport as Stream;
use rmux_proto::{
  ClientMessage, CommandSpec, DEFAULT_PRESENTATION_WINDOW_BYTES, PROTOCOL_VERSION, ServerMessage,
  TerminalSize, read_frame, write_frame,
};
use std::time::Duration;
use tokio::time::{Instant, timeout};

struct Gateway;
impl Gateway {
  async fn connect(&self) -> Stream {
    let host = std::env::var("CTL_TEST_SSH_HOST").expect("set CTL_TEST_SSH_HOST");
    let options = ctl_core::SshConnectionOptions {
      remote_platform: ctl_core::RemotePlatform::Windows,
      ..ctl_core::SshConnectionOptions::default()
    };
    let mut stream = timeout(
      Duration::from_secs(20),
      ctl_core::open_ssh_tunnel_interactive(&host, &options, &ctl_core::SshInteraction::Batch),
    )
    .await
    .expect("SSH connection timed out")
    .expect("open authenticated gateway");
    write_frame(
      &mut stream,
      &ClientMessage::Handshake {
        protocol_version: PROTOCOL_VERSION,
        client_name: "windows-ssh-test".into(),
        client_version: "test".into(),
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      message(&mut stream).await,
      ServerMessage::HandshakeAccepted { .. }
    ));
    stream
  }
  async fn create(&self) -> String {
    let mut stream = self.connect().await;
    write_frame(
      &mut stream,
      &ClientMessage::CreateSession {
        name: Some(format!("ssh-{}", uuid::Uuid::new_v4())),
        command: Some(CommandSpec {
          program: "cmd.exe".into(),
          arguments: vec!["/D".into(), "/Q".into()],
        }),
        working_directory: None,
        terminal_size: size(),
      },
    )
    .await
    .unwrap();
    let ServerMessage::SessionCreated { session } = message(&mut stream).await else {
      panic!("session not created")
    };
    session.session_id
  }
  async fn attach(&self, session: &str) -> (Stream, String) {
    let mut stream = self.connect().await;
    write_frame(
      &mut stream,
      &ClientMessage::AttachSession {
        session: session.into(),
        resume_from: None,
        terminal_size: size(),
        request_input_lease: true,
        request_layout_lease: true,
        request_command_line: false,
        request_running_command: false,
        presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
      },
    )
    .await
    .unwrap();
    let token = attached(&mut stream).await;
    (stream, token)
  }
}
fn size() -> TerminalSize {
  TerminalSize {
    columns: 80,
    rows: 24,
    pixel_width: 0,
    pixel_height: 0,
  }
}
async fn message(stream: &mut Stream) -> ServerMessage {
  let message = timeout(Duration::from_secs(15), read_frame(stream))
    .await
    .expect("protocol timed out")
    .unwrap()
    .expect("unexpected EOF");
  assert!(
    !matches!(message, ServerMessage::Error { .. }),
    "{message:?}"
  );
  message
}
async fn attached(stream: &mut Stream) -> String {
  let ServerMessage::Attached {
    attachment_token,
    checkpoint,
    ..
  } = message(stream).await
  else {
    panic!("not attached")
  };
  if let Some(checkpoint) = checkpoint {
    write_frame(
      stream,
      &ClientMessage::PresentationApplied {
        sequence: checkpoint.sequence,
      },
    )
    .await
    .unwrap();
  }
  attachment_token
}
async fn input(stream: &mut Stream, command: &str) {
  write_frame(
    stream,
    &ClientMessage::Input {
      data: command.as_bytes().to_vec(),
    },
  )
  .await
  .unwrap();
}
async fn output_until(stream: &mut Stream, marker: &str) -> u64 {
  let deadline = Instant::now() + Duration::from_secs(15);
  let mut output = Vec::new();
  loop {
    assert!(
      Instant::now() < deadline,
      "output marker missing: {output:?}"
    );
    match message(stream).await {
      ServerMessage::Output {
        data, sequence_end, ..
      } => {
        eprintln!("ConPTY output: {:?}", String::from_utf8_lossy(&data));
        output.extend(data);
        write_frame(
          stream,
          &ClientMessage::PresentationApplied {
            sequence: sequence_end,
          },
        )
        .await
        .unwrap();
        if String::from_utf8_lossy(&output).contains(marker) {
          return sequence_end;
        }
      }
      ServerMessage::Checkpoint { checkpoint, .. } => {
        write_frame(
          stream,
          &ClientMessage::PresentationApplied {
            sequence: checkpoint.sequence,
          },
        )
        .await
        .unwrap();
      }
      ServerMessage::SessionEnded { .. } => panic!("session ended before marker: {output:?}"),
      _ => {}
    }
  }
}

// Final-output fixtures must fit within one presentation window and stay below
// the checkpoint threshold. The SSH channel may already have closed its input,
// so drain without acknowledgements and require the final SessionEnded.
async fn assert_final_output(stream: &mut Stream, marker: &str, expected_exit_code: u32) {
  let deadline = Instant::now() + Duration::from_secs(15);
  let mut output = Vec::new();
  let mut marker_seen = false;
  loop {
    assert!(Instant::now() < deadline, "session did not end: {output:?}");
    match message(stream).await {
      ServerMessage::Output { data, .. } => output.extend(data),
      ServerMessage::Checkpoint { checkpoint, .. } => output = checkpoint.payload,
      ServerMessage::SessionEnded { exit_code, .. } => {
        assert!(marker_seen, "final output marker missing: {output:?}");
        assert_eq!(exit_code, Some(expected_exit_code));
        return;
      }
      _ => {}
    }
    marker_seen |= String::from_utf8_lossy(&output).contains(marker);
  }
}

#[tokio::test]
#[ignore = "requires an authenticated Windows OpenSSH fixture; run windows-ssh.ps1"]
async fn authenticated_ssh_survives_disconnect_resizes_and_drains_exit_output() {
  let daemon = Gateway;
  let session = daemon.create().await;
  let (mut first, token) = daemon.attach(&session).await;
  input(&mut first, "echo BEFORE_DISCONNECT\r").await;
  let sequence = output_until(&mut first, "BEFORE_DISCONNECT").await;
  drop(first);
  let mut resumed = daemon.connect().await;
  write_frame(
    &mut resumed,
    &ClientMessage::ResumeAttachment {
      session,
      attachment_token: token,
      resume_from: Some(sequence),
      terminal_size: size(),
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await
  .unwrap();
  attached(&mut resumed).await;
  let resized = TerminalSize {
    columns: 100,
    rows: 30,
    ..size()
  };
  write_frame(
    &mut resumed,
    &ClientMessage::Resize {
      terminal_size: resized,
    },
  )
  .await
  .unwrap();
  input(&mut resumed, "echo AFTER_RESUME\r").await;
  output_until(&mut resumed, "AFTER_RESUME").await;
  input(&mut resumed, "echo FINAL_TAIL& exit /b 7\r").await;
  assert_final_output(&mut resumed, "FINAL_TAIL", 7).await;
  drop(resumed);
}
