#![cfg(windows)]

use rmux_ipc::{
  Stream, connect_existing_daemon, control_socket_path, request_local_daemon_restart,
};
use rmux_proto::{
  ClientMessage, CommandSpec, DEFAULT_PRESENTATION_WINDOW_BYTES, PROTOCOL_VERSION, ServerMessage,
  TerminalSize, read_frame, write_frame,
};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
  process::{Child, Command},
  time::{Instant, sleep, timeout},
};
use uuid::Uuid;

struct Daemon {
  child: Child,
  socket: PathBuf,
}
impl Daemon {
  async fn start() -> Self {
    let socket = PathBuf::from(format!(r"\\.\pipe\rmux-conpty-test-{}", Uuid::new_v4()));
    let child = Command::new(env!("CARGO_BIN_EXE_rmuxd"))
      .arg("--socket")
      .arg(&socket)
      .arg("--startup-idle-seconds")
      .arg("30")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::inherit())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let daemon = Self { child, socket };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
      if connect_existing_daemon(&daemon.socket).await.is_ok() {
        break;
      }
      assert!(Instant::now() < deadline, "daemon never opened its pipe");
      sleep(Duration::from_millis(25)).await;
    }
    daemon
  }
  async fn connect(&self) -> Stream {
    let mut stream = connect_existing_daemon(&self.socket).await.unwrap();
    write_frame(
      &mut stream,
      &ClientMessage::Handshake {
        protocol_version: PROTOCOL_VERSION,
        client_name: "windows-test".into(),
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
        name: Some("console".into()),
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
  async fn wait(mut self) {
    let status = timeout(Duration::from_secs(15), self.child.wait())
      .await
      .expect("daemon did not exit")
      .unwrap();
    assert!(status.success());
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

#[tokio::test]
async fn conpty_survives_disconnect_resizes_and_drains_exit_output() {
  let daemon = Daemon::start().await;
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
  output_until(&mut resumed, "FINAL_TAIL").await;
  loop {
    if let ServerMessage::SessionEnded { exit_code, .. } = message(&mut resumed).await {
      assert_eq!(exit_code, Some(7));
      break;
    }
  }
  drop(resumed);
  daemon.wait().await;
}

#[tokio::test]
async fn local_restart_closes_conpty_and_releases_both_endpoints() {
  let daemon = Daemon::start().await;
  let session = daemon.create().await;
  let (mut stream, _) = daemon.attach(&session).await;
  input(&mut stream, "echo READY_FOR_RESTART\r").await;
  output_until(&mut stream, "READY_FOR_RESTART").await;
  let control = control_socket_path(&daemon.socket).unwrap();
  let connection = connect_existing_daemon(&control).await.unwrap();
  let count = timeout(
    Duration::from_secs(15),
    request_local_daemon_restart(connection),
  )
  .await
  .expect("restart timed out")
  .unwrap();
  assert_eq!(count, 1);
  drop(stream);
  let socket = daemon.socket.clone();
  daemon.wait().await;
  assert!(connect_existing_daemon(&socket).await.is_err());
  assert!(connect_existing_daemon(&control).await.is_err());
}
