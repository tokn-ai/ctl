use super::Result;
use ctmux_proto::{ClientMessage, CommandSpec, ServerMessage, SplitAxis, TerminalSize, ViewInfo};
use std::{
  fs::DirBuilder,
  os::unix::fs::DirBuilderExt,
  path::{Path, PathBuf},
  sync::mpsc::{self, Receiver, TryRecvError},
  thread::{self, JoinHandle},
  time::Duration,
};
use tokio::time::{Instant, sleep, timeout};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// A daemon with its own runtime, sockets, and controlled shell fixtures.
///
/// Its runtime outlives an unwinding test until cooperative shutdown has killed
/// and reaped the persistent shell processes. Aborting just the accept loop
/// would leave those shells alive.
pub struct TestDaemon {
  pub directory: PathBuf,
  pub socket: PathBuf,
  task: Option<JoinHandle<()>>,
  completed: Receiver<std::result::Result<(), String>>,
}

impl TestDaemon {
  pub async fn start() -> Result<Self> {
    Self::start_with_config(ctmuxd::DaemonConfig::default()).await
  }

  /// Choose a bounded reconnect lifetime for token-expiry and ownership cases.
  pub async fn start_with_liveness(attachment_liveness_timeout: Duration) -> Result<Self> {
    Self::start_with_config(ctmuxd::DaemonConfig {
      attachment_liveness_timeout,
      ..Default::default()
    })
    .await
  }

  async fn start_with_config(mut config: ctmuxd::DaemonConfig) -> Result<Self> {
    // Keep paths short enough for macOS's Unix-domain socket limit.
    let directory =
      std::env::temp_dir().join(format!("ctui-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    DirBuilder::new().mode(0o700).create(&directory)?;
    if let Err(error) = DirBuilder::new().mode(0o700).create(directory.join("home")) {
      let _ = std::fs::remove_dir_all(&directory);
      return Err(error.into());
    }
    let socket = directory.join("ctmux.sock");
    config.socket_path.clone_from(&socket);
    config.startup_idle_timeout = Duration::from_secs(60);
    let (sender, completed) = mpsc::channel();
    let task = match thread::Builder::new()
      .name("tui-fixture-daemon".into())
      .spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
          .enable_all()
          .build()
          .map_err(|error| error.to_string())
          .and_then(|runtime| {
            let result = runtime
              .block_on(ctmuxd::run(config))
              .map_err(|error| error.to_string());
            runtime.shutdown_timeout(Duration::from_secs(1));
            result
          });
        let _ = sender.send(result);
      }) {
      Ok(task) => task,
      Err(error) => {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error.into());
      }
    };
    let daemon = Self {
      directory,
      socket,
      task: Some(task),
      completed,
    };
    timeout(REQUEST_TIMEOUT, async {
      loop {
        if daemon.task.as_ref().is_some_and(JoinHandle::is_finished) {
          return Result::<()>::Err("fixture daemon exited before becoming ready".into());
        }
        // bind creates the path before listen makes it connectable. A complete
        // request proves readiness; path existence alone can race with startup.
        match tokio::net::UnixStream::connect(&daemon.socket).await {
          Ok(stream) => {
            let response = Self::exchange(stream, ClientMessage::ListSessions).await?;
            return match response {
              ServerMessage::SessionList { .. } => Ok(()),
              response => {
                Err(format!("unexpected fixture readiness response: {response:?}").into())
              }
            };
          }
          Err(error)
            if matches!(
              error.kind(),
              std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
          {
            sleep(Duration::from_millis(10)).await;
          }
          Err(error) => return Err(error.into()),
        }
      }
    })
    .await
    .map_err(|_| "fixture daemon did not become ready")??;
    Ok(daemon)
  }

  pub async fn request(&self, message: ClientMessage) -> Result<ServerMessage> {
    timeout(REQUEST_TIMEOUT, async {
      // Never fall back to starting the user's installed daemon.
      let stream = ctmux_ipc::connect_existing_daemon(&self.socket).await?;
      Self::exchange(stream, message).await
    })
    .await
    .map_err(|_| "fixture daemon request timed out")?
  }

  async fn exchange(stream: ctmux_ipc::Stream, message: ClientMessage) -> Result<ServerMessage> {
    ctmux_client::request(
      stream,
      &ctmux_client::ClientIdentity {
        name: "tui-process-test".into(),
        version: "test".into(),
      },
      message,
    )
    .await
    .map_err(Into::into)
  }

  pub async fn create_echo_session(
    &self,
    name: &str,
    tag: &str,
    terminal_size: TerminalSize,
  ) -> Result<String> {
    match self
      .request(ClientMessage::CreateSession {
        name: Some(name.into()),
        command: Some(echo_command(tag)),
        working_directory: Some(self.directory.to_string_lossy().into_owned()),
        terminal_size,
      })
      .await?
    {
      ServerMessage::SessionCreated { session } => Ok(session.session_id),
      response => Err(format!("expected session_created, received {response:?}").into()),
    }
  }

  pub async fn split_echo(
    &self,
    terminal_id: &str,
    axis: SplitAxis,
    tag: &str,
    terminal_size: TerminalSize,
  ) -> Result<ViewInfo> {
    match self
      .request(ClientMessage::SplitTerminal {
        terminal_id: terminal_id.into(),
        axis,
        command: Some(echo_command(tag)),
        working_directory: Some(self.directory.to_string_lossy().into_owned()),
        terminal_size,
      })
      .await?
    {
      ServerMessage::ViewSnapshot { view } => Ok(view),
      response => Err(format!("expected view_snapshot, received {response:?}").into()),
    }
  }

  pub async fn shutdown(&mut self) -> Result<()> {
    if self.task.is_none() {
      return Ok(());
    }
    if !self.task.as_ref().is_some_and(JoinHandle::is_finished) {
      request_shutdown(&self.socket).await?;
    }
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    loop {
      match self.completed.try_recv() {
        Ok(result) => return self.finish(result),
        Err(TryRecvError::Disconnected) => {
          return self.finish(Err("fixture daemon thread panicked".into()));
        }
        Err(TryRecvError::Empty) if Instant::now() < deadline => {
          sleep(Duration::from_millis(10)).await;
        }
        Err(TryRecvError::Empty) => {
          return Err("fixture daemon did not exit after cooperative shutdown".into());
        }
      }
    }
  }

  fn finish(&mut self, result: std::result::Result<(), String>) -> Result<()> {
    if let Some(task) = self.task.take() {
      task.join().map_err(|_| "fixture daemon thread panicked")?;
    }
    result.map_err(Into::into)
  }
}

impl Drop for TestDaemon {
  fn drop(&mut self) {
    if self.task.is_some() {
      let socket = self.socket.clone();
      // Drop may run inside a test runtime or after it has stopped. A separate
      // runtime avoids nested block_on and keeps shutdown independent of both.
      let shutdown = thread::Builder::new()
        .name("tui-fixture-cleanup".into())
        .spawn(move || {
          let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
          let result = runtime
            .block_on(request_shutdown(&socket))
            .map_err(|error| error.to_string());
          runtime.shutdown_timeout(Duration::from_secs(1));
          result
        });
      match shutdown {
        Ok(shutdown) => {
          if let Ok(Err(error)) = shutdown.join() {
            // An idle daemon may already have exited naturally.
            if self.socket.exists() {
              eprintln!("fixture daemon cleanup request failed: {error}");
            }
          }
        }
        Err(error) => eprintln!("could not start fixture daemon cleanup: {error}"),
      }
      let result = self.completed.recv_timeout(SHUTDOWN_TIMEOUT);
      match result {
        Ok(result) => {
          if let Err(error) = self.finish(result) {
            eprintln!("fixture daemon cleanup failed: {error}");
          }
        }
        Err(error) => eprintln!("fixture daemon cleanup did not complete: {error}"),
      }
    }
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

async fn request_shutdown(socket: &Path) -> Result<()> {
  let control = ctmux_ipc::control_socket_path(socket)?;
  timeout(SHUTDOWN_TIMEOUT, async {
    let stream = tokio::net::UnixStream::connect(control).await?;
    ctmux_ipc::request_local_daemon_restart(stream).await?;
    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
  })
  .await
  .map_err(|_| "fixture daemon shutdown request timed out")?
}

fn echo_command(tag: &str) -> CommandSpec {
  CommandSpec {
    program: "/bin/sh".into(),
    arguments: vec![
      "-c".into(),
      "PATH=/usr/bin:/bin; export PATH; stty -echo; printf '%s:ready\\n' \"$1\"; while IFS= read -r line; do printf '%s:%s\\n' \"$1\" \"$line\"; done".into(),
      "ctmux-fixture".into(),
      tag.into(),
    ],
  }
}
