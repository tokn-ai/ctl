#![cfg(unix)]
use ctl_core::observability::{Component, Event, Level, Outcome, Store, Stream as HistoryStream};
use ctmux_proto::{
  ClientMessage, CommandSpec, ServerMessage, TerminalSize, read_frame, write_frame,
};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::time::{Instant, sleep, timeout};
use uuid::Uuid;

struct Daemon {
  child: Child,
  directory: PathBuf,
  socket: PathBuf,
}
impl Daemon {
  async fn start(level: Option<&str>) -> Self {
    let directory =
      std::env::temp_dir().join(format!("cl-{}", &Uuid::new_v4().simple().to_string()[..16]));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = directory.join("s");
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctmuxd"));
    command
      .arg("--detach-from-terminal")
      .args(["--startup-idle-seconds", "30"])
      .arg("--socket")
      .arg(&socket)
      .env("HOME", &directory)
      .env_remove("CTL_LOG_LEVEL")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::piped());
    if let Some(level) = level {
      command.env("CTL_LOG_LEVEL", level);
    }
    let mut daemon = Self {
      child: command.spawn().unwrap(),
      directory,
      socket,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&daemon.socket).await.is_err() {
      if let Some(status) = daemon.child.try_wait().unwrap() {
        use std::io::Read as _;
        let mut stderr = String::new();
        daemon
          .child
          .stderr
          .take()
          .unwrap()
          .read_to_string(&mut stderr)
          .unwrap();
        panic!("ctmuxd exited before becoming ready: {status}: {stderr}");
      }
      assert!(Instant::now() < deadline, "ctmuxd did not become ready");
      sleep(Duration::from_millis(10)).await;
    }
    daemon
  }

  async fn connect(&self) -> UnixStream {
    self.connect_version(None).await
  }

  async fn connect_version(
    &self,
    version: Option<ctl_core::protocol::ProtocolVersion>,
  ) -> UnixStream {
    let mut stream = UnixStream::connect(&self.socket).await.unwrap();
    write_frame(
      &mut stream,
      &ClientMessage::Handshake {
        protocol: version.map_or_else(ctmux_proto::protocol_offer, |version| {
          ctl_core::protocol::ProtocolOffer::new(version.build, version, &[version])
        }),
        client_name: "log-test".into(),
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

  async fn request(&self, request: ClientMessage) -> ServerMessage {
    let mut stream = self.connect().await;
    write_frame(&mut stream, &request).await.unwrap();
    message(&mut stream).await
  }

  async fn wait(&mut self) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
      if let Some(status) = self.child.try_wait().unwrap() {
        assert!(status.success());
        break;
      }
      assert!(
        Instant::now() < deadline,
        "ctmuxd did not exit after killing its session"
      );
      sleep(Duration::from_millis(10)).await;
    }
  }
}
impl Drop for Daemon {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
    let _ = fs::remove_dir_all(&self.directory);
  }
}

async fn message(stream: &mut UnixStream) -> ServerMessage {
  timeout(Duration::from_secs(10), read_frame(stream))
    .await
    .unwrap()
    .unwrap()
    .unwrap()
}

fn command() -> CommandSpec {
  CommandSpec {
    program: "/bin/cat".into(),
    arguments: Vec::new(),
  }
}

async fn exercise(daemon: &Daemon) -> String {
  let ServerMessage::SessionCreated { session } = daemon
    .request(ClientMessage::CreateSession {
      name: Some("log-private-canary".into()),
      command: Some(command()),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    })
    .await
  else {
    panic!("session not created")
  };
  let ServerMessage::ViewSnapshot { .. } = daemon
    .request(ClientMessage::SplitTerminal {
      terminal_id: session.terminal_id.clone(),
      axis: ctmux_proto::SplitAxis::Horizontal,
      command: Some(command()),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    })
    .await
  else {
    panic!("pane not split")
  };
  let mut stream = daemon.connect().await;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: session.session_id.clone(),
      resume_from: None,
      terminal_size: TerminalSize::default(),
      request_input_lease: true,
      request_layout_lease: true,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: ctmux_proto::DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await
  .unwrap();
  let ServerMessage::Attached {
    attachment_token, ..
  } = message(&mut stream).await
  else {
    panic!("not attached")
  };
  write_frame(
    &mut stream,
    &ClientMessage::Resize {
      terminal_size: TerminalSize {
        columns: 100,
        rows: 30,
        ..TerminalSize::default()
      },
    },
  )
  .await
  .unwrap();
  write_frame(&mut stream, &ClientMessage::Heartbeat { nonce: 1 })
    .await
    .unwrap();
  // Use an ordered acknowledgement as the barrier, not a sleep after resize.
  loop {
    if matches!(
      message(&mut stream).await,
      ServerMessage::HeartbeatAck { nonce: 1, .. }
    ) {
      break;
    }
  }
  write_frame(&mut stream, &ClientMessage::Detach)
    .await
    .unwrap();
  loop {
    if matches!(message(&mut stream).await, ServerMessage::Detached) {
      break;
    }
  }
  drop(stream);
  assert!(matches!(
    daemon
      .request(ClientMessage::KillSession {
        session: session.session_id
      })
      .await,
    ServerMessage::Success
  ));
  attachment_token
}

#[tokio::test]
async fn heartbeat_history_statistics_require_the_selected_new_contract() {
  let daemon = Daemon::start(None).await;
  let ServerMessage::SessionCreated { session } = daemon
    .request(ClientMessage::CreateSession {
      name: Some("heartbeat-statistics".into()),
      command: Some(command()),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    })
    .await
  else {
    panic!("session not created");
  };
  for &version in ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS {
    let mut stream = daemon.connect_version(Some(version)).await;
    write_frame(
      &mut stream,
      &ClientMessage::AttachSession {
        session: session.session_id.clone(),
        resume_from: None,
        terminal_size: TerminalSize::default(),
        request_input_lease: false,
        request_layout_lease: false,
        request_command_line: false,
        request_running_command: false,
        presentation_window_bytes: ctmux_proto::DEFAULT_PRESENTATION_WINDOW_BYTES,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      message(&mut stream).await,
      ServerMessage::Attached { .. }
    ));
    write_frame(&mut stream, &ClientMessage::Heartbeat { nonce: 77 })
      .await
      .unwrap();
    loop {
      if let ServerMessage::HeartbeatAck {
        nonce,
        session_history,
      } = message(&mut stream).await
      {
        assert_eq!(nonce, 77);
        if ctmux_proto::supports_session_statistics(version) {
          let statistics = session_history.expect("new contract supplies current session history");
          assert_eq!(statistics.session_id, session.session_id);
          assert_eq!(statistics.retained_bytes, 0);
        } else {
          assert!(
            session_history.is_none(),
            "legacy heartbeat changed under {version}"
          );
        }
        break;
      }
    }
    write_frame(&mut stream, &ClientMessage::Detach)
      .await
      .unwrap();
    loop {
      if matches!(message(&mut stream).await, ServerMessage::Detached) {
        break;
      }
    }
  }
}

#[tokio::test]
async fn sessions_and_panes_write_redacted_human_logs_with_default_and_trace_levels() {
  for level in [None, Some("trace")] {
    let mut daemon = Daemon::start(level).await;
    let token = exercise(&daemon).await;
    daemon.wait().await;
    let store = Store::new(daemon.directory.join(".tokn/ctl/history"));
    let history = store.read(HistoryStream::Logs, 1000, false).unwrap();
    assert!(history.complete);
    for event in [
      Event::DaemonLifecycle,
      Event::SessionCreate,
      Event::PaneSplit,
      Event::AttachmentCreate,
      Event::AttachmentDetach,
      Event::SessionTerminate,
      Event::PaneKill,
      Event::PaneExit,
    ] {
      assert!(
        history
          .records
          .iter()
          .any(|record| record.event == event && record.outcome == Outcome::Succeeded),
        "missing {event:?}"
      );
    }
    assert!(
      history
        .records
        .iter()
        .all(|record| record.component == Component::Ctmuxd)
    );
    let created = history
      .records
      .iter()
      .find(|record| record.event == Event::SessionCreate && record.outcome == Outcome::Succeeded)
      .unwrap();
    assert!(created.context.session_id.is_some());
    assert!(created.context.pane_id.is_some());
    assert_eq!(
      history
        .records
        .iter()
        .any(|record| record.event == Event::ViewResize && record.level == Level::Debug),
      level.is_some()
    );
    assert_eq!(
      history
        .records
        .iter()
        .any(|record| record.level == Level::Trace),
      level.is_some()
    );
    for event in [Event::PaneExit, Event::AttachmentDetach] {
      assert!(
        !history
          .records
          .iter()
          .any(|record| record.event == event && record.outcome == Outcome::Started)
      );
    }
    assert!(!store.directory().join("audit.sqlite3").exists());
    for entry in fs::read_dir(store.directory().join("logs")).unwrap() {
      let path = entry.unwrap().path();
      if path.extension().is_some_and(|extension| extension == "log") {
        let text = fs::read_to_string(path).unwrap();
        assert!(
          text
            .lines()
            .all(|line| line.contains('T') && line.contains("Z ") && !line.starts_with('{'))
        );
        assert!(text.lines().all(|line| line.contains("\tmessage=")));
        assert!(!text.contains("log-private-canary"));
        assert!(!text.contains(&token));
        assert!(!text.contains("/bin/cat"));
      }
    }
  }
}
