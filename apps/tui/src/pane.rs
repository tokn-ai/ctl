use crate::{Result, model::Model, transport::Transport};
use ctmux_client::{
  AttachRequest, AttachmentAcknowledgementError, AttachmentControl, AttachmentController,
  AttachmentControllerOptions, AttachmentEvent, AttachmentEvents, ClientIdentity,
  DEFAULT_PRESENTATION_WINDOW_BYTES,
};
use ctmux_proto::{
  ErrorCode, LeaseKind, ResizeDirection, TerminalCheckpoint, TerminalHistoryRow, TerminalSize,
  ViewInfo,
};
use std::{
  collections::{BTreeMap, VecDeque},
  time::Duration,
};
use tokio::task::JoinHandle;

/// Fresh-attachment preferences and explicit changes made before reconnect.
#[derive(Clone, Copy)]
pub struct ReconnectLeases {
  pub input: bool,
  pub layout: bool,
  input_change: Option<bool>,
  layout_change: Option<bool>,
}

impl ReconnectLeases {
  pub const fn new(input: bool, layout: bool) -> Self {
    Self {
      input,
      layout,
      input_change: None,
      layout_change: None,
    }
  }

  pub const fn requested(self, lease: LeaseKind) -> bool {
    match lease {
      LeaseKind::Input => self.input,
      LeaseKind::Layout => self.layout,
    }
  }

  const fn explicit_change(self, lease: LeaseKind) -> Option<bool> {
    match lease {
      LeaseKind::Input => self.input_change,
      LeaseKind::Layout => self.layout_change,
    }
  }

  pub fn intended_ownership(self, lease: LeaseKind, observed: bool) -> bool {
    self.explicit_change(lease).unwrap_or(observed)
  }
}

pub struct Pane {
  pub model: Model,
  pub control: AttachmentControl,
  pub connected: bool,
  pub ended: Option<String>,
  pub view_update: Option<ViewInfo>,
  pending_resizes: BTreeMap<String, tokio::time::Instant>,
  events: AttachmentEvents,
  pub token: String,
  pub reconnect_leases: ReconnectLeases,
  runner: Option<JoinHandle<()>>,
  checkpoint_ready: bool,
  sequence: u64,
  history_snapshot_id: Option<String>,
  history_boundary: u64,
  replay: VecDeque<ReplayChunk>,
  replay_bytes: usize,
  history_job: Option<JoinHandle<std::io::Result<PreparedHistory>>>,
}

const MAX_HISTORY_REPLAY_BYTES: usize = 4 * 1024 * 1024;

fn accept_buffered_ack(
  result: std::result::Result<(), AttachmentAcknowledgementError>,
) -> Result<()> {
  // The controller can queue final presentation events and SessionEnded
  // before the renderer drains them. A closed acknowledgement endpoint must
  // not discard those events or their final screen; ordering errors still fail.
  match result {
    Ok(()) | Err(AttachmentAcknowledgementError::Closed) => Ok(()),
    Err(error) => Err(error.into()),
  }
}

#[derive(Clone)]
struct ReplayChunk {
  start: u64,
  end: u64,
  data: Vec<u8>,
}

struct PreparedHistory {
  snapshot_id: String,
  sequence: u64,
  vt: avt::Vt,
  pending: Vec<u8>,
  history_gap: bool,
}

fn replay_history(
  mut prepared: PreparedHistory,
  chunks: Vec<ReplayChunk>,
) -> std::io::Result<PreparedHistory> {
  for chunk in chunks {
    if chunk.start != prepared.sequence {
      return Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "history replay has a gap",
      ));
    }
    prepared.history_gap |= ctmux_client::history::feed_projection_with_evictions(
      &mut prepared.vt,
      &mut prepared.pending,
      &chunk.data,
    )?;
    prepared.sequence = chunk.end;
  }
  Ok(prepared)
}

pub fn identity() -> ClientIdentity {
  ClientIdentity {
    name: "ctmux-tui".into(),
    version: env!("CARGO_PKG_VERSION").into(),
  }
}

impl Pane {
  #[cfg(test)]
  pub(crate) async fn wait_for_controller_exit(&mut self) {
    if let Some(runner) = self.runner.take() {
      runner.await.expect("attachment controller task panicked");
    }
  }

  pub async fn open(
    transport: &dyn Transport,
    terminal_id: &str,
    size: TerminalSize,
    leases: ReconnectLeases,
    token: Option<String>,
  ) -> Result<Self> {
    let stream = transport.connect().await?;
    let request = AttachRequest {
      session: terminal_id.into(),
      resume_from: None,
      terminal_size: size,
      request_input_lease: leases.input,
      request_layout_lease: leases.layout,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    };
    let resuming = token.is_some();
    let attached = if let Some(token) = token {
      ctmux_client::resume_attach(stream, &identity(), token, request.clone()).await
    } else {
      ctmux_client::begin_attach(stream, &identity(), request.clone()).await
    };
    let (stream, attached) = match attached {
      Ok(attached) => attached,
      Err(ctmux_client::ClientError::Server {
        code: ErrorCode::AttachmentResumeRejected,
        ..
      }) if resuming => {
        let stream = transport.connect().await?;
        ctmux_client::begin_attach(stream, &identity(), request).await?
      }
      Err(error) => return Err(error.into()),
    };
    let token = attached.attachment_token.clone();
    let model = Model::new(&attached.session.terminal_size);
    let (controller, control, events) =
      AttachmentController::new(stream, &attached, AttachmentControllerOptions::default())?;
    let runner = tokio::spawn(async move {
      let _ = controller.run().await;
    });
    let mut pane = Self {
      model,
      control,
      connected: true,
      ended: None,
      view_update: None,
      pending_resizes: BTreeMap::new(),
      events,
      token,
      reconnect_leases: leases,
      runner: Some(runner),
      checkpoint_ready: false,
      sequence: attached.replay_from,
      history_snapshot_id: None,
      history_boundary: attached.replay_from,
      replay: VecDeque::new(),
      replay_bytes: 0,
      history_job: None,
    };
    pane.prepare_reconnect().await?;
    Ok(pane)
  }

  async fn prepare_reconnect(&mut self) -> Result<()> {
    // ResumeAttachment preserves server leases; its AttachRequest flags are
    // intentionally ignored. Apply explicit changes queued while disconnected
    // before exposing the replacement controller to foreground input.
    for lease in [LeaseKind::Input, LeaseKind::Layout] {
      let status = self.lease_status(lease);
      if !self.reconnect_leases.requested(lease) && status.owned_by_client {
        self.control.release_lease(lease).await?;
      } else if self.reconnect_leases.explicit_change(lease) == Some(true) && !status.held {
        self.control.acquire_lease(lease).await?;
      }
    }
    while !self.checkpoint_ready || self.lease_intent_pending() {
      if let Some(error) = self.drain().await? {
        return Err(error.into());
      }
      if !self.connected {
        if self.checkpoint_ready && self.ended.is_some() {
          break;
        }
        return Err("Disconnected before receiving terminal checkpoint".into());
      }
      if !self.checkpoint_ready || self.lease_intent_pending() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
      }
    }
    self.reconnect_leases.input_change = None;
    self.reconnect_leases.layout_change = None;
    Ok(())
  }

  fn lease_intent_pending(&self) -> bool {
    [LeaseKind::Input, LeaseKind::Layout]
      .into_iter()
      .any(|lease| {
        let status = self.lease_status(lease);
        (!self.reconnect_leases.requested(lease) && status.owned_by_client)
          || (self.reconnect_leases.explicit_change(lease) == Some(true) && !status.held)
      })
  }

  fn lease_status(&self, lease: LeaseKind) -> ctmux_proto::LeaseStatus {
    let leases = self.control.state().leases();
    match lease {
      LeaseKind::Input => leases.input,
      LeaseKind::Layout => leases.layout,
    }
  }

  pub fn request_lease(&mut self, lease: LeaseKind, requested: bool) {
    match lease {
      LeaseKind::Input => {
        self.reconnect_leases.input = requested;
        self.reconnect_leases.input_change = Some(requested);
      }
      LeaseKind::Layout => {
        self.reconnect_leases.layout = requested;
        self.reconnect_leases.layout_change = Some(requested);
      }
    }
  }

  pub async fn drain(&mut self) -> Result<Option<String>> {
    let mut message = None;
    // Fairness: a noisy PTY must not starve input or sibling renderers.
    for _ in 0..64 {
      let event = match self.events.try_recv() {
        Ok(event) => event,
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
          self.connected = false;
          break;
        }
      };
      if let Some(notice) = self.apply_event(event).await? {
        message = Some(notice);
      }
    }
    self.publish_ready_history().await;
    if self.connected {
      let count = self.pending_resizes.len();
      self
        .pending_resizes
        .retain(|_, queued| queued.elapsed() < Duration::from_secs(5));
      if self.pending_resizes.len() != count {
        message.get_or_insert_with(|| "Pane resize acknowledgement timed out".into());
      }
    } else {
      self.pending_resizes.clear();
    }
    Ok(message)
  }

  pub async fn resize_pane(
    &mut self,
    terminal_id: String,
    direction: ResizeDirection,
    amount: u16,
    request_id: String,
  ) -> Result<()> {
    if self.pending_resizes.len() >= 32 {
      return Err("Waiting for earlier pane resize requests".into());
    }
    tokio::time::timeout(
      Duration::from_millis(100),
      self
        .control
        .resize_pane(terminal_id, direction, amount, request_id.clone()),
    )
    .await
    .map_err(|_| "Pane resize command queue is busy")??;
    self
      .pending_resizes
      .insert(request_id, tokio::time::Instant::now());
    Ok(())
  }

  async fn apply_event(&mut self, event: AttachmentEvent) -> Result<Option<String>> {
    match event {
      AttachmentEvent::Checkpoint {
        checkpoint,
        history,
        history_manifest,
        history_gap,
      } => {
        self.cancel_history();
        self.sequence = checkpoint.sequence;
        self.history_boundary = checkpoint.sequence;
        self.history_snapshot_id = history_manifest.map(|manifest| manifest.snapshot_id);
        self.model.restore(&checkpoint);
        self.checkpoint_ready = true;
        self.model.history_gap = history_gap || history.truncated;
        self.model.set_history(history.lines);
        accept_buffered_ack(
          self
            .control
            .acknowledge_checkpoint(checkpoint.sequence)
            .await,
        )?;
      }
      AttachmentEvent::Output {
        data,
        sequence_start,
        sequence_end,
      } => {
        let reply = self.model.feed(&data);
        self.sequence = sequence_end;
        if self.history_snapshot_id.is_some() {
          self.replay_bytes += data.len();
          self.replay.push_back(ReplayChunk {
            start: sequence_start,
            end: sequence_end,
            data,
          });
          if self.replay_bytes > MAX_HISTORY_REPLAY_BYTES {
            self.model.history_gap = true;
            self.cancel_history();
            let _ignored = self.control.request_checkpoint().await;
          }
        }
        accept_buffered_ack(self.control.acknowledge_output(sequence_end).await)?;
        self.reply_to_terminal(reply).await?;
      }
      AttachmentEvent::PtyGeometryChanged {
        terminal_size,
        observed_sequence,
      } => {
        self.cancel_history();
        self.model.resize(&terminal_size);
        accept_buffered_ack(self.control.acknowledge_geometry(observed_sequence).await)?;
      }
      AttachmentEvent::HistorySynced {
        snapshot_id,
        checkpoint,
        rows,
        scrollback_limit,
        history_gap,
        ..
      } => {
        self.start_history_transfer(snapshot_id, checkpoint, rows, scrollback_limit, history_gap);
      }
      AttachmentEvent::ViewChanged { view } => self.queue_view_update(view),
      AttachmentEvent::PaneResizeResult {
        request_id,
        outcome,
      } => {
        return Ok(self.apply_resize_result(&request_id, outcome));
      }
      AttachmentEvent::ServerError { message: error, .. } => return Ok(Some(error)),
      AttachmentEvent::SessionEnded { exit_code, .. } => {
        self.connected = false;
        self.ended = Some(exit_code.map_or_else(
          || "Terminal ended".into(),
          |code| format!("Exited (code {code})"),
        ));
      }
      AttachmentEvent::Exited { .. } => self.connected = false,
      _ => {}
    }
    Ok(None)
  }

  fn start_history_transfer(
    &mut self,
    snapshot_id: String,
    checkpoint: TerminalCheckpoint,
    rows: Vec<TerminalHistoryRow>,
    scrollback_limit: u64,
    history_gap: bool,
  ) {
    if self.history_snapshot_id.as_deref() != Some(snapshot_id.as_str())
      || checkpoint.sequence != self.history_boundary
    {
      return;
    }
    let chunks = self.replay.iter().cloned().collect();
    if let Some(job) = self.history_job.take() {
      job.abort();
    }
    self.history_job = Some(tokio::task::spawn_blocking(move || {
      let vt = ctmux_client::history::restore_projection(&checkpoint, &rows, scrollback_limit)?;
      replay_history(
        PreparedHistory {
          snapshot_id,
          sequence: checkpoint.sequence,
          vt,
          pending: checkpoint.input_prefix,
          history_gap,
        },
        chunks,
      )
    }));
  }

  fn apply_resize_result(
    &mut self,
    request_id: &str,
    outcome: ctmux_proto::PaneResizeOutcome,
  ) -> Option<String> {
    self.pending_resizes.remove(request_id)?;
    match outcome {
      ctmux_proto::PaneResizeOutcome::Applied { view } => {
        self.queue_view_update(*view);
        None
      }
      ctmux_proto::PaneResizeOutcome::Rejected { message, .. } => Some(message),
    }
  }

  async fn reply_to_terminal(&self, reply: Vec<u8>) -> Result<()> {
    if !reply.is_empty() && self.control.state().leases().input.owned_by_client {
      // Final output may request a terminal reply after the reader has
      // already queued SessionEnded and closed its command endpoint.
      if let Err(error) = self.control.input(reply).await
        && error != ctmux_client::AttachmentCommandError::Closed
      {
        return Err(error.into());
      }
    }
    Ok(())
  }

  fn queue_view_update(&mut self, view: ViewInfo) {
    if self
      .view_update
      .as_ref()
      .is_none_or(|pending| view.revision >= pending.revision)
    {
      self.view_update = Some(view);
    }
  }

  async fn publish_ready_history(&mut self) {
    if self
      .history_job
      .as_ref()
      .is_some_and(tokio::task::JoinHandle::is_finished)
    {
      let job = self
        .history_job
        .take()
        .expect("completed history job exists");
      self.publish_history_result(job.await).await;
    }
  }

  async fn publish_history_result(
    &mut self,
    result: std::result::Result<std::io::Result<PreparedHistory>, tokio::task::JoinError>,
  ) {
    match result {
      Ok(Ok(prepared))
        if self.history_snapshot_id.as_deref() == Some(prepared.snapshot_id.as_str()) =>
      {
        if prepared.sequence == self.sequence {
          if !self.model.adopt_history_projection(
            prepared.vt,
            prepared.pending,
            prepared.history_gap,
          ) {
            self.model.history_gap = true;
            let _ignored = self.control.request_checkpoint().await;
          }
          self.cancel_history();
        } else {
          let chunks = self
            .replay
            .iter()
            .filter(|chunk| chunk.start >= prepared.sequence)
            .cloned()
            .collect();
          self.history_job = Some(tokio::task::spawn_blocking(move || {
            replay_history(prepared, chunks)
          }));
        }
      }
      Ok(Ok(_)) => {}
      _ => {
        self.model.history_gap = true;
        self.cancel_history();
        let _ignored = self.control.request_checkpoint().await;
      }
    }
  }

  fn cancel_history(&mut self) {
    self.history_snapshot_id = None;
    self.replay.clear();
    self.replay_bytes = 0;
    if let Some(job) = self.history_job.take() {
      job.abort();
    }
  }

  pub fn history_gap(&self) -> bool {
    self.model.history_gap || self.history_snapshot_id.is_some()
  }

  pub fn history_status(&self) -> &'static str {
    if self.history_snapshot_id.is_some() {
      "history syncing"
    } else if self.model.history_gap {
      "history incomplete"
    } else {
      "history ready"
    }
  }

  pub async fn finish_ended_history(&mut self) {
    if self.ended.is_none() {
      return;
    }
    while let Some(job) = self.history_job.take() {
      self.publish_history_result(job.await).await;
    }
  }

  pub async fn close(&mut self) {
    self.cancel_history();
    if self.connected {
      let _ = self.control.detach().await;
    }
    if let Some(mut runner) = self.runner.take()
      && tokio::time::timeout(std::time::Duration::from_secs(1), &mut runner)
        .await
        .is_err()
    {
      runner.abort();
    }
  }
}

impl Drop for Pane {
  fn drop(&mut self) {
    if let Some(job) = &self.history_job {
      job.abort();
    }
    if let Some(runner) = &self.runner {
      runner.abort();
    }
  }
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use crate::test_daemon;

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn resize_results_require_a_pending_id_and_expire_without_disconnect() -> Result<()> {
    let mut daemon = test_daemon::TestDaemon::start().await?;
    let session = daemon
      .create_echo_session("resize-correlation", "first", TerminalSize::default())
      .await?;
    let transport = crate::transport::LocalTransport(daemon.socket.clone());
    let mut pane = Pane::open(
      &transport,
      &session,
      TerminalSize::default(),
      ReconnectLeases::new(true, true),
      None,
    )
    .await?;
    let rejected = ctmux_proto::PaneResizeOutcome::Rejected {
      code: ErrorCode::InvalidRequest,
      message: "no matching divider".into(),
    };
    assert!(
      pane
        .apply_event(AttachmentEvent::PaneResizeResult {
          request_id: "unknown".into(),
          outcome: rejected.clone()
        })
        .await?
        .is_none()
    );
    pane
      .pending_resizes
      .insert("expected".into(), tokio::time::Instant::now());
    assert_eq!(
      pane
        .apply_event(AttachmentEvent::PaneResizeResult {
          request_id: "expected".into(),
          outcome: rejected
        })
        .await?,
      Some("no matching divider".into())
    );
    assert!(pane.connected);
    assert!(pane.pending_resizes.is_empty());
    pane.pending_resizes.insert(
      "expired".into(),
      tokio::time::Instant::now() - Duration::from_secs(6),
    );
    assert_eq!(
      pane.drain().await?,
      Some("Pane resize acknowledgement timed out".into())
    );
    assert!(pane.pending_resizes.is_empty());
    assert!(pane.connected);
    pane.close().await;
    drop(pane);
    daemon.shutdown().await?;
    Ok(())
  }
}
