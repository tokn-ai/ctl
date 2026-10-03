use crate::{Result, model::Model, transport::Transport};
use ctmux_client::{
  AttachRequest, AttachmentControl, AttachmentController, AttachmentControllerOptions,
  AttachmentEvent, AttachmentEvents, ClientIdentity, DEFAULT_PRESENTATION_WINDOW_BYTES,
};
use ctmux_proto::TerminalSize;
use std::collections::VecDeque;
use tokio::task::JoinHandle;

pub struct Pane {
  pub model: Model,
  pub control: AttachmentControl,
  pub connected: bool,
  pub ended: Option<String>,
  events: AttachmentEvents,
  pub token: String,
  runner: Option<JoinHandle<()>>,
  sequence: u64,
  history_snapshot_id: Option<String>,
  history_boundary: u64,
  replay: VecDeque<ReplayChunk>,
  replay_bytes: usize,
  history_job: Option<JoinHandle<std::io::Result<PreparedHistory>>>,
}

const MAX_HISTORY_REPLAY_BYTES: usize = 4 * 1024 * 1024;

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
  pub async fn open(
    transport: &dyn Transport,
    terminal_id: &str,
    size: TerminalSize,
    read_only: bool,
    layout: bool,
    token: Option<String>,
  ) -> Result<Self> {
    let stream = transport.connect().await?;
    let request = AttachRequest {
      session: terminal_id.into(),
      resume_from: None,
      terminal_size: size,
      request_input_lease: !read_only,
      request_layout_lease: layout && !read_only,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
    };
    let attached = if let Some(token) = token {
      ctmux_client::resume_attach(stream, &identity(), token, request.clone()).await
    } else {
      ctmux_client::begin_attach(stream, &identity(), request.clone()).await
    };
    let (stream, attached) = match attached {
      Ok(attached) => attached,
      Err(ctmux_client::ClientError::Server { .. }) => {
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
    Ok(Self {
      model,
      control,
      connected: true,
      ended: None,
      events,
      token,
      runner: Some(runner),
      sequence: attached.replay_from,
      history_snapshot_id: None,
      history_boundary: attached.replay_from,
      replay: VecDeque::new(),
      replay_bytes: 0,
      history_job: None,
    })
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
    Ok(message)
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
        self.model.history_gap = history_gap || history.truncated;
        self.model.set_history(history.lines);
        self
          .control
          .acknowledge_checkpoint(checkpoint.sequence)
          .await?;
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
        self.control.acknowledge_output(sequence_end).await?;
        if !reply.is_empty() && self.control.state().leases().input.owned_by_client {
          self.control.input(reply).await?;
        }
      }
      AttachmentEvent::PtyGeometryChanged {
        terminal_size,
        observed_sequence,
      } => {
        self.cancel_history();
        self.model.resize(&terminal_size);
        self.control.acknowledge_geometry(observed_sequence).await?;
      }
      AttachmentEvent::HistorySynced {
        snapshot_id,
        checkpoint,
        rows,
        scrollback_limit,
        history_gap,
        ..
      } => {
        if self.history_snapshot_id.as_deref() != Some(snapshot_id.as_str())
          || checkpoint.sequence != self.history_boundary
        {
          return Ok(None);
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
