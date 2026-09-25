#![cfg(unix)]
use rmux_proto::{
  ClientMessage, CommandSpec, PROTOCOL_VERSION, ServerMessage, TerminalEndReason, TerminalSize,
  read_frame, write_frame,
};
use rmuxd::{DaemonConfig, run};
use std::os::unix::fs::PermissionsExt;
use std::{
  path::{Path, PathBuf},
  time::Duration,
};
use tokio::{
  net::UnixStream,
  time::{sleep, timeout},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Directory(PathBuf);
impl Directory {
  fn new() -> std::io::Result<Self> {
    let directory = Directory(std::env::temp_dir().join(format!(
      "rarchive-{}",
      &uuid::Uuid::new_v4().to_string()[..8]
    )));
    std::fs::create_dir(&directory.0)?;
    std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o700))?;
    Ok(directory)
  }
}
impl Drop for Directory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

async fn request(socket: &Path, message: ClientMessage) -> Result<ServerMessage> {
  let mut stream = timeout(Duration::from_secs(5), async {
    loop {
      match UnixStream::connect(socket).await {
        Ok(stream) => break stream,
        Err(_) => sleep(Duration::from_millis(10)).await,
      }
    }
  })
  .await?;
  write_frame(
    &mut stream,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: "archive-test".into(),
      client_version: "test".into(),
    },
  )
  .await?;
  let _: ServerMessage = read_frame(&mut stream).await?.ok_or("handshake closed")?;
  write_frame(&mut stream, &message).await?;
  Ok(
    timeout(Duration::from_secs(5), read_frame(&mut stream))
      .await??
      .ok_or("response closed")?,
  )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_and_deleted_sessions_survive_restart_as_expiring_read_only_archives() -> Result {
  let directory = Directory::new()?;
  let socket = directory.0.join("rmux.sock");
  let config = DaemonConfig {
    socket_path: socket.clone(),
    ..DaemonConfig::default()
  };
  let daemon = tokio::spawn(run(config.clone()));
  let ServerMessage::SessionCreated { session } = request(
    &socket,
    ClientMessage::CreateSession {
      name: Some("completed".into()),
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "printf 'final-output'; exit 7".into()],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?
  else {
    panic!("session not created");
  };
  timeout(Duration::from_secs(5), daemon).await???;

  let restarted = tokio::spawn(run(config));
  let ServerMessage::ArchiveList { archives } =
    request(&socket, ClientMessage::ListArchives).await?
  else {
    panic!("archive list");
  };
  assert_eq!(archives.len(), 1);
  let archive = &archives[0];
  assert_eq!(archive.session.session_id, session.session_id);
  assert_eq!(
    archive.expires_at_ms - archive.archived_at_ms,
    7 * 86_400_000
  );
  assert_eq!(archive.terminals[0].exit_code, Some(7));
  assert_eq!(archive.terminals[0].reason, TerminalEndReason::Exited);
  let ServerMessage::ArchivedTerminalSnapshot { terminal } = request(
    &socket,
    ClientMessage::GetArchivedTerminal {
      session_id: session.session_id.clone(),
      terminal_id: session.terminal_id.clone(),
    },
  )
  .await?
  else {
    panic!("terminal snapshot");
  };
  assert!(String::from_utf8_lossy(&terminal.checkpoint.payload).contains("final-output"));
  assert!(
    matches!(request(&socket, ClientMessage::ListSessions).await?, ServerMessage::SessionList { sessions } if sessions.is_empty())
  );
  assert!(matches!(
    request(
      &socket,
      ClientMessage::KillSession {
        session: session.session_id.clone()
      }
    )
    .await?,
    ServerMessage::Error {
      code: rmux_proto::ErrorCode::SessionNotFound,
      ..
    }
  ));

  let deleted = create_and_delete_split(&socket).await?;
  timeout(Duration::from_secs(5), restarted).await???;
  let index = directory
    .0
    .join("rmux.sock.archives")
    .join(&deleted.session_id)
    .join("session.json");
  let record: rmux_proto::SessionArchive = serde_json::from_slice(&std::fs::read(&index)?)?;
  assert_eq!(record.terminals.len(), 2);
  assert_eq!(record.view.panes.len(), 2);
  assert!(
    record
      .terminals
      .iter()
      .all(|terminal| terminal.reason == TerminalEndReason::Terminated)
  );
  let mut expired = record;
  expired.expires_at_ms = 1;
  std::fs::write(&index, serde_json::to_vec(&expired)?)?;
  let last = tokio::spawn(run(DaemonConfig {
    socket_path: socket.clone(),
    ..DaemonConfig::default()
  }));
  let response = request(&socket, ClientMessage::ListArchives).await?;
  assert!(matches!(response, ServerMessage::ArchiveList { archives } if archives.len() == 1));
  assert!(!index.exists());
  last.abort();
  let _ = last.await;
  Ok(())
}

async fn create_and_delete_split(socket: &Path) -> Result<rmux_proto::SessionInfo> {
  let ServerMessage::SessionCreated { session: deleted } = request(
    socket,
    ClientMessage::CreateSession {
      name: Some("deleted".into()),
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "read -r line".into()],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?
  else {
    panic!("create deleted");
  };
  let split = request(
    socket,
    ClientMessage::SplitTerminal {
      terminal_id: deleted.terminal_id.clone(),
      axis: rmux_proto::SplitAxis::Horizontal,
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "read -r line".into()],
      }),
      working_directory: None,
      terminal_size: TerminalSize::default(),
    },
  )
  .await?;
  assert!(matches!(split, ServerMessage::ViewSnapshot { view } if view.terminals.len() == 2));
  assert!(matches!(
    request(
      socket,
      ClientMessage::KillSession {
        session: deleted.session_id.clone()
      }
    )
    .await?,
    ServerMessage::Success
  ));

  Ok(deleted)
}
