#![cfg(unix)]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::{io::Read, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use tokio::time::{sleep, timeout};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Harness {
  directory: PathBuf,
  daemon: tokio::task::JoinHandle<std::result::Result<(), rmuxd::DaemonError>>,
  child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
}

impl Drop for Harness {
  fn drop(&mut self) {
    if let Some(child) = &mut self.child {
      let _ = child.kill();
      let _ = child.wait();
    }
    self.daemon.abort();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

fn read_output(
  mut reader: Box<dyn Read + Send>,
  stop_when_ready: bool,
) -> (
  tokio::sync::oneshot::Receiver<()>,
  tokio::task::JoinHandle<std::io::Result<()>>,
) {
  let (sender, receiver) = tokio::sync::oneshot::channel();
  let task = tokio::task::spawn_blocking(move || {
    let mut sender = Some(sender);
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    loop {
      let count = reader.read(&mut buffer)?;
      if count == 0 {
        return Ok(());
      }
      if sender.is_some() {
        output.extend_from_slice(&buffer[..count]);
        if output
          .windows(b"\x1b[?2004h".len())
          .any(|bytes| bytes == b"\x1b[?2004h")
        {
          let _ = sender.take().unwrap().send(());
          if stop_when_ready {
            return Ok(());
          }
          output.clear();
        }
      }
    }
  });
  (receiver, task)
}

async fn check_shutdown(disconnect: bool) -> Result<()> {
  let directory = std::env::temp_dir().join(format!(
    "rtui-tty-{}",
    &uuid::Uuid::new_v4().to_string()[..8]
  ));
  std::fs::create_dir(&directory)?;
  std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
  let socket = directory.join("rmux.sock");
  let daemon = tokio::spawn(rmuxd::run(rmuxd::DaemonConfig {
    socket_path: socket.clone(),
    ..Default::default()
  }));
  let mut harness = Harness {
    directory,
    daemon,
    child: None,
  };
  timeout(Duration::from_secs(5), async {
    while !socket.exists() {
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .map_err(|_| "daemon socket did not become ready")?;

  let pair = native_pty_system().openpty(PtySize {
    rows: 25,
    cols: 80,
    pixel_width: 0,
    pixel_height: 0,
  })?;
  let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_rmux-tui"));
  command.arg("--socket");
  command.arg(&socket);
  command.env("TERM", "xterm-256color");
  command.env("SHELL", "/bin/sh");
  let child = pair.slave.spawn_command(command)?;
  harness.child = Some(child);
  drop(pair.slave);
  let reader = pair.master.try_clone_reader()?;
  let (ready, output) = read_output(reader, disconnect);
  timeout(Duration::from_secs(5), ready)
    .await
    .map_err(|_| "TUI did not enter raw mode")??;
  if disconnect {
    output.await??;
  }
  // Let the input thread enter its poll before revoking the terminal.
  sleep(Duration::from_millis(100)).await;
  if !disconnect {
    use std::io::Write;
    pair.master.take_writer()?.write_all(b"\x02d")?;
  }
  let master = if disconnect {
    drop(pair.master);
    None
  } else {
    Some(pair.master)
  };
  let status = timeout(Duration::from_secs(3), async {
    loop {
      if let Some(status) = harness.child.as_mut().unwrap().try_wait()? {
        return Ok::<_, std::io::Error>(status);
      }
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await??;
  if !disconnect {
    assert!(status.success(), "prefix detach must exit cleanly");
  }
  harness.child = None;
  drop(master);
  // Losing the client terminal must not terminate the daemon's persistent PTY.
  let stream = rmux_ipc::connect_or_start_daemon(&socket).await?;
  let response = rmux_client::request(
    stream,
    &rmux_client::ClientIdentity {
      name: "terminal-shutdown-test".into(),
      version: "test".into(),
    },
    rmux_proto::ClientMessage::ListSessions,
  )
  .await?;
  let rmux_proto::ServerMessage::SessionList { sessions } = response else {
    panic!("expected session list")
  };
  assert_eq!(sessions.len(), 1);
  let stream = rmux_ipc::connect_or_start_daemon(&socket).await?;
  rmux_client::request(
    stream,
    &rmux_client::ClientIdentity {
      name: "terminal-shutdown-test".into(),
      version: "test".into(),
    },
    rmux_proto::ClientMessage::KillSession {
      session: sessions[0].session_id.clone(),
    },
  )
  .await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_the_host_terminal_does_not_leave_a_spinning_tui() -> Result<()> {
  check_shutdown(true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prefix_detach_stops_the_reader_and_preserves_the_session() -> Result<()> {
  check_shutdown(false).await
}
