use crate::{Result, model::Model, transport::Transport};
use ctmux_client::{
  AttachRequest, AttachmentAcknowledgementError, AttachmentControl, AttachmentController,
  AttachmentControllerOptions, AttachmentEvent, AttachmentEvents, ClientIdentity,
  DEFAULT_PRESENTATION_WINDOW_BYTES,
};
use ctmux_proto::{
  DividerResize, ErrorCode, LeaseKind, PaneResizeOutcome, ResizeDirection, TerminalCheckpoint,
  TerminalHistoryRow, TerminalSize, ViewInfo,
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
  /// Results for locally queued requests, including acknowledgement timeouts.
  pub resize_results: VecDeque<(String, PaneResizeOutcome)>,
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
const MAX_PENDING_RESIZES: usize = 32;
const RESIZE_QUEUE_TIMEOUT: Duration = Duration::from_millis(100);
const RESIZE_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const RESIZE_TIMEOUT_MESSAGE: &str = "Pane resize acknowledgement timed out";

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
      resize_results: VecDeque::new(),
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
      let expired: Vec<_> = self
        .pending_resizes
        .iter()
        .filter(|(_, queued)| queued.elapsed() >= RESIZE_ACK_TIMEOUT)
        .map(|(request_id, _)| request_id.clone())
        .collect();
      for request_id in expired {
        if let Some(notice) = self.apply_resize_result(
          &request_id,
          PaneResizeOutcome::Rejected {
            code: ErrorCode::Internal,
            message: RESIZE_TIMEOUT_MESSAGE.into(),
          },
        ) {
          message.get_or_insert(notice);
        }
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
    let control = self.control.clone();
    self
      .queue_resize(
        request_id.clone(),
        control.resize_pane(terminal_id, direction, amount, request_id),
      )
      .await
  }

  pub async fn resize_divider(&mut self, divider: DividerResize, request_id: String) -> Result<()> {
    let control = self.control.clone();
    self
      .queue_resize(
        request_id.clone(),
        control.resize_divider(divider, request_id),
      )
      .await
  }

  async fn queue_resize(
    &mut self,
    request_id: String,
    command: impl std::future::Future<
      Output = std::result::Result<(), ctmux_client::AttachmentCommandError>,
    >,
  ) -> Result<()> {
    if self.pending_resizes.len() >= MAX_PENDING_RESIZES {
      return Err("Waiting for earlier pane resize requests".into());
    }
    tokio::time::timeout(RESIZE_QUEUE_TIMEOUT, command)
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
    outcome: PaneResizeOutcome,
  ) -> Option<String> {
    self.pending_resizes.remove(request_id)?;
    self
      .resize_results
      .push_back((request_id.into(), outcome.clone()));
    match outcome {
      PaneResizeOutcome::Applied { view } => {
        self.queue_view_update(*view);
        None
      }
      PaneResizeOutcome::Rejected { message, .. } => Some(message),
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
    assert!(pane.resize_results.is_empty());
    pane
      .pending_resizes
      .insert("expected".into(), tokio::time::Instant::now());
    assert_eq!(
      pane
        .apply_event(AttachmentEvent::PaneResizeResult {
          request_id: "expected".into(),
          outcome: rejected.clone()
        })
        .await?,
      Some("no matching divider".into())
    );
    assert!(pane.connected);
    assert!(pane.pending_resizes.is_empty());
    assert_eq!(
      pane.resize_results.pop_front(),
      Some(("expected".into(), rejected))
    );
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
    assert_eq!(
      pane.resize_results.pop_front(),
      Some((
        "expired".into(),
        PaneResizeOutcome::Rejected {
          code: ErrorCode::Internal,
          message: RESIZE_TIMEOUT_MESSAGE.into(),
        }
      ))
    );
    assert!(
      pane
        .apply_resize_result(
          "expired",
          PaneResizeOutcome::Rejected {
            code: ErrorCode::InvalidRequest,
            message: "late acknowledgement".into(),
          },
        )
        .is_none()
    );
    assert!(pane.resize_results.is_empty());
    pane.close().await;
    drop(pane);
    daemon.shutdown().await?;
    Ok(())
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn divider_resize_updates_view_and_rejects_unsupported_or_busy_commands() -> Result<()> {
    let mut daemon = test_daemon::TestDaemon::start().await?;
    let session = daemon
      .create_echo_session("divider-correlation", "first", TerminalSize::default())
      .await?;
    let ctmux_proto::ServerMessage::ViewSnapshot { view } = daemon
      .request(ctmux_proto::ClientMessage::GetView {
        session: session.clone(),
      })
      .await?
    else {
      return Err("expected initial view".into());
    };
    let terminal_id = view.terminals[0].terminal_id.clone();
    daemon
      .split_echo(
        &terminal_id,
        ctmux_proto::SplitAxis::Horizontal,
        "second",
        TerminalSize::default(),
      )
      .await?;
    let transport = crate::transport::LocalTransport(daemon.socket.clone());
    let mut pane = Pane::open(
      &transport,
      &terminal_id,
      TerminalSize::default(),
      ReconnectLeases::new(true, true),
      None,
    )
    .await?;
    let ctmux_proto::ServerMessage::ViewSnapshot { view } = daemon
      .request(ctmux_proto::ClientMessage::GetView {
        session: session.clone(),
      })
      .await?
    else {
      return Err("expected split view".into());
    };
    let divider = DividerResize {
      view_id: view.view_id,
      expected_revision: view.revision,
      split_path: Vec::new(),
      boundary: 0,
      position: view.panes[0].columns + 3,
    };
    pane
      .resize_divider(divider.clone(), "divider-1".into())
      .await?;
    tokio::time::timeout(Duration::from_secs(3), async {
      while pane.resize_results.is_empty() {
        if let Some(notice) = pane.drain().await? {
          return Err(notice.into());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
      Result::Ok(())
    })
    .await??;
    let Some((request_id, PaneResizeOutcome::Applied { view })) = pane.resize_results.pop_front()
    else {
      return Err("expected applied divider result".into());
    };
    assert_eq!(request_id, "divider-1");
    assert_eq!(view.panes[0].columns, divider.position);
    assert_eq!(pane.view_update.as_ref(), Some(view.as_ref()));
    assert!(pane.pending_resizes.is_empty());

    assert_resize_command_limits(&mut pane, &transport, terminal_id, divider).await?;
    pane.close().await;
    drop(pane);
    daemon.shutdown().await?;
    Ok(())
  }

  async fn assert_resize_command_limits(
    pane: &mut Pane,
    transport: &dyn Transport,
    terminal_id: String,
    divider: DividerResize,
  ) -> Result<()> {
    // Negotiate a separate attachment, then use its contract and paused command
    // queue to exercise the TUI adapter without depending on daemon timing.
    let (stream, mut attached) = ctmux_client::begin_attach(
      transport.connect().await?,
      &identity(),
      AttachRequest {
        session: terminal_id,
        resume_from: None,
        terminal_size: TerminalSize::default(),
        request_input_lease: false,
        request_layout_lease: false,
        request_command_line: false,
        request_running_command: false,
        presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
      },
    )
    .await?;
    attached.handshake_info.protocol_version = ctmux_proto::CONTRACT_V1_1_16;
    let (legacy_controller, legacy_control, _events) =
      AttachmentController::new(stream, &attached, AttachmentControllerOptions::default())?;
    let active_control = std::mem::replace(&mut pane.control, legacy_control);
    let error = pane
      .resize_divider(divider.clone(), "legacy-divider".into())
      .await
      .expect_err("contract 16 cannot resize an exact divider");
    assert_eq!(
      error.downcast_ref::<ctmux_client::AttachmentCommandError>(),
      Some(&ctmux_client::AttachmentCommandError::DividerResizeUnavailable)
    );
    assert!(pane.pending_resizes.is_empty());
    drop(legacy_controller);

    attached.handshake_info.protocol_version = ctmux_proto::PROTOCOL_VERSION;
    attached.layout_lease = ctmux_proto::LeaseStatus {
      held: true,
      owned_by_client: true,
    };
    let (stream, _peer) = tokio::io::duplex(4096);
    let (paused_controller, paused_control, _events) = AttachmentController::new(
      stream,
      &attached,
      AttachmentControllerOptions {
        command_queue_capacity: 1,
        ..Default::default()
      },
    )?;
    pane.control = paused_control;
    pane
      .resize_divider(divider.clone(), "queued-divider".into())
      .await?;
    assert!(pane.pending_resizes.contains_key("queued-divider"));
    let error = pane
      .resize_divider(divider.clone(), "blocked-divider".into())
      .await
      .expect_err("a full command queue must not stall the TUI");
    assert_eq!(error.to_string(), "Pane resize command queue is busy");
    assert!(!pane.pending_resizes.contains_key("blocked-divider"));
    for index in 1..MAX_PENDING_RESIZES {
      pane
        .pending_resizes
        .insert(format!("pending-{index}"), tokio::time::Instant::now());
    }
    let error = pane
      .resize_divider(divider, "excess-divider".into())
      .await
      .expect_err("pending resize acknowledgements must stay bounded");
    assert_eq!(
      error.to_string(),
      "Waiting for earlier pane resize requests"
    );
    assert_eq!(pane.pending_resizes.len(), MAX_PENDING_RESIZES);
    pane.pending_resizes.clear();
    pane.control = active_control;
    drop(paused_controller);
    Ok(())
  }
}
