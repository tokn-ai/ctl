use super::{Result, TestDaemon};
use ctmux_proto::{ClientMessage, ErrorCode, ServerMessage, read_frame, write_frame};
use std::{
  collections::VecDeque,
  os::unix::fs::PermissionsExt,
  path::PathBuf,
  sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
  },
  time::Duration,
};
use tokio::{
  net::{UnixListener, UnixStream},
  sync::{Notify, watch},
  task::{JoinHandle, JoinSet},
  time::timeout,
};

#[derive(Clone, Copy, Default)]
struct State {
  generation: u64,
  paused: bool,
  hold_move_results: bool,
}

#[derive(Default)]
struct Progress {
  attachments: AtomicUsize,
  resumptions: AtomicUsize,
  stalled: AtomicUsize,
  resume_rejections: AtomicUsize,
  held_move_results: AtomicUsize,
  changed: Notify,
}

/// A private transport boundary that can cut live streams or hold new handshakes.
pub struct TestProxy {
  pub socket: PathBuf,
  state: watch::Sender<State>,
  progress: Arc<Progress>,
  task: JoinHandle<()>,
}

impl TestProxy {
  pub fn start(daemon: &TestDaemon) -> Result<Self> {
    let socket = daemon.directory.join("proxy.sock");
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let upstream = daemon.socket.clone();
    let (state, receiver) = watch::channel(State::default());
    let progress = Arc::new(Progress::default());
    let observed = Arc::clone(&progress);
    let task = tokio::spawn(async move {
      let mut relays = JoinSet::new();
      loop {
        tokio::select! {
          accepted = listener.accept() => {
            let Ok((stream, _)) = accepted else { break; };
            let upstream = upstream.clone();
            let state = receiver.clone();
            let progress = Arc::clone(&observed);
            relays.spawn(async move {
              run_relay(stream, upstream, state, progress).await;
            });
          }
          _ = relays.join_next(), if !relays.is_empty() => {}
        }
      }
    });
    Ok(Self {
      socket,
      state,
      progress,
      task,
    })
  }

  pub fn attachments(&self) -> usize {
    self.progress.attachments.load(Ordering::Acquire)
  }

  pub fn stalled(&self) -> usize {
    self.progress.stalled.load(Ordering::Acquire)
  }

  pub fn resume_rejections(&self) -> usize {
    self.progress.resume_rejections.load(Ordering::Acquire)
  }

  pub fn resumptions(&self) -> usize {
    self.progress.resumptions.load(Ordering::Acquire)
  }

  pub fn held_move_results(&self) -> usize {
    self.progress.held_move_results.load(Ordering::Acquire)
  }

  /// Hold correlated move replies while forwarding view updates and live traffic.
  pub fn hold_move_results(&self) {
    self
      .state
      .send_modify(|state| state.hold_move_results = true);
  }

  pub fn release_move_results(&self) {
    self
      .state
      .send_modify(|state| state.hold_move_results = false);
  }

  pub async fn wait_held_move_result(&self, previous: usize) -> Result<()> {
    self
      .wait_progress("a pane move reply held by the proxy", |progress| {
        progress.held_move_results.load(Ordering::Acquire) > previous
      })
      .await
  }

  /// Close every active generation while holding replacement handshakes.
  pub fn interrupt(&self) {
    self.state.send_modify(|state| {
      state.generation += 1;
      state.paused = true;
    });
  }

  /// Keep live panes forwarding, but hold subsequent request handshakes.
  pub fn stall_requests(&self) {
    self.state.send_modify(|state| state.paused = true);
  }

  pub fn resume(&self) {
    self.state.send_modify(|state| state.paused = false);
  }

  pub async fn wait_stalled(&self, previous: usize) -> Result<()> {
    self
      .wait_progress("a received handshake held by the proxy", |progress| {
        progress.stalled.load(Ordering::Acquire) > previous
      })
      .await
  }

  pub async fn wait_attachments(&self, count: usize) -> Result<()> {
    self
      .wait_progress("replacement attachments", |progress| {
        progress.attachments.load(Ordering::Acquire) >= count
      })
      .await
  }

  async fn wait_progress(
    &self,
    description: &str,
    ready: impl Fn(&Progress) -> bool,
  ) -> Result<()> {
    timeout(Duration::from_secs(5), async {
      loop {
        let notified = self.progress.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if ready(&self.progress) {
          return;
        }
        notified.await;
      }
    })
    .await
    .map_err(|_| {
      format!(
        "waiting for {description}; completed attachments {}, stalled handshakes {}",
        self.attachments(),
        self.progress.stalled.load(Ordering::Acquire),
      )
      .into()
    })
  }
}

async fn run_relay(
  stream: UnixStream,
  upstream: PathBuf,
  mut state: watch::Receiver<State>,
  progress: Arc<Progress>,
) {
  let generation = state.borrow().generation;
  let relay = forward(stream, upstream, state.clone(), progress);
  tokio::pin!(relay);
  loop {
    tokio::select! {
      _ = &mut relay => break,
      changed = state.changed() => {
        if changed.is_err() || state.borrow().generation != generation {
          break;
        }
      }
    }
  }
}

async fn forward(
  mut stream: UnixStream,
  upstream: PathBuf,
  mut state: watch::Receiver<State>,
  progress: Arc<Progress>,
) -> Result<()> {
  let hello = read_frame::<_, ClientMessage>(&mut stream)
    .await?
    .ok_or("proxy client closed before its handshake")?;
  if !matches!(hello, ClientMessage::Handshake { .. }) {
    return Err("proxy expected a client handshake".into());
  }
  if state.borrow().paused {
    progress.stalled.fetch_add(1, Ordering::Release);
    progress.changed.notify_waiters();
    while state.borrow().paused {
      state.changed().await?;
    }
  }
  let mut daemon = UnixStream::connect(upstream).await?;
  write_frame(&mut daemon, &hello).await?;
  let welcome = read_frame::<_, ServerMessage>(&mut daemon)
    .await?
    .ok_or("proxy daemon closed before accepting handshake")?;
  write_frame(&mut stream, &welcome).await?;
  let request = read_frame::<_, ClientMessage>(&mut stream)
    .await?
    .ok_or("proxy client closed before request")?;
  write_frame(&mut daemon, &request).await?;
  let response = read_frame::<_, ServerMessage>(&mut daemon)
    .await?
    .ok_or("proxy daemon closed before request response")?;
  write_frame(&mut stream, &response).await?;
  if matches!(response, ServerMessage::Attached { .. }) {
    if matches!(request, ClientMessage::ResumeAttachment { .. }) {
      progress.resumptions.fetch_add(1, Ordering::Release);
    }
    progress.attachments.fetch_add(1, Ordering::Release);
    progress.changed.notify_waiters();
  } else if matches!(
    response,
    ServerMessage::Error {
      code: ErrorCode::AttachmentResumeRejected,
      ..
    }
  ) {
    progress.resume_rejections.fetch_add(1, Ordering::Release);
    progress.changed.notify_waiters();
  }
  relay_messages(stream, daemon, state, progress).await
}

async fn relay_messages(
  stream: UnixStream,
  daemon: UnixStream,
  mut state: watch::Receiver<State>,
  progress: Arc<Progress>,
) -> Result<()> {
  let (mut client_reader, mut client_writer) = stream.into_split();
  let (mut daemon_reader, mut daemon_writer) = daemon.into_split();
  let client_to_daemon = async move {
    while let Some(message) = read_frame::<_, ClientMessage>(&mut client_reader).await? {
      write_frame(&mut daemon_writer, &message).await?;
    }
    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
  };
  let daemon_to_client = async move {
    let mut held = VecDeque::new();
    loop {
      if !state.borrow().hold_move_results {
        while let Some(message) = held.pop_front() {
          write_frame(&mut client_writer, &message).await?;
        }
      }
      // Keep an in-progress frame read alive when hold/release state changes;
      // cancelling read_frame after its length prefix would corrupt the stream.
      let reading = read_frame::<_, ServerMessage>(&mut daemon_reader);
      tokio::pin!(reading);
      let message = loop {
        tokio::select! {
          message = &mut reading => break message?,
          changed = state.changed() => {
            if changed.is_err() { return Ok(()); }
            if !state.borrow().hold_move_results {
              while let Some(message) = held.pop_front() {
                write_frame(&mut client_writer, &message).await?;
              }
            }
          }
        }
      };
      let Some(message) = message else {
        break;
      };
      if matches!(message, ServerMessage::PaneMoveResult { .. }) && state.borrow().hold_move_results
      {
        if held.len() >= 64 {
          return Err("proxy held too many pane move replies".into());
        }
        held.push_back(message);
        progress.held_move_results.fetch_add(1, Ordering::Release);
        progress.changed.notify_waiters();
      } else {
        write_frame(&mut client_writer, &message).await?;
      }
    }
    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
  };
  tokio::try_join!(client_to_daemon, daemon_to_client)?;
  Ok(())
}

impl Drop for TestProxy {
  fn drop(&mut self) {
    // Dropping the listener's JoinSet also cancels every child relay.
    self.task.abort();
    let _ = std::fs::remove_file(&self.socket);
  }
}
