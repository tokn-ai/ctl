use crate::{Result, model::Model};
use rmux_client::{
  AttachRequest, AttachmentControl, AttachmentController, AttachmentControllerOptions,
  AttachmentEvent, AttachmentEvents, ClientIdentity, DEFAULT_PRESENTATION_WINDOW_BYTES,
};
use rmux_proto::TerminalSize;
use std::path::Path;
use tokio::task::JoinHandle;

pub struct Pane {
  pub model: Model,
  pub control: AttachmentControl,
  pub connected: bool,
  events: AttachmentEvents,
  pub token: String,
  runner: Option<JoinHandle<()>>,
}

pub fn identity() -> ClientIdentity {
  ClientIdentity {
    name: "rmux-tui".into(),
    version: env!("CARGO_PKG_VERSION").into(),
  }
}

impl Pane {
  pub async fn open(
    socket: &Path,
    terminal_id: &str,
    size: TerminalSize,
    read_only: bool,
    layout: bool,
    token: Option<String>,
  ) -> Result<Self> {
    let stream = rmux_ipc::connect_or_start_daemon(socket).await?;
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
      rmux_client::resume_attach(stream, &identity(), token, request.clone()).await
    } else {
      rmux_client::begin_attach(stream, &identity(), request.clone()).await
    };
    let (stream, attached) = match attached {
      Ok(attached) => attached,
      Err(rmux_client::ClientError::Server { .. }) => {
        let stream = rmux_ipc::connect_or_start_daemon(socket).await?;
        rmux_client::begin_attach(stream, &identity(), request).await?
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
      events,
      token,
      runner: Some(runner),
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
      match event {
        AttachmentEvent::Checkpoint { checkpoint, .. } => {
          self.model.restore(&checkpoint);
          self
            .control
            .acknowledge_checkpoint(checkpoint.sequence)
            .await?;
        }
        AttachmentEvent::Output {
          data, sequence_end, ..
        } => {
          let reply = self.model.feed(&data);
          self.control.acknowledge_output(sequence_end).await?;
          if !reply.is_empty() && self.control.state().leases().input.owned_by_client {
            self.control.input(reply).await?;
          }
        }
        AttachmentEvent::PtyGeometryChanged {
          terminal_size,
          observed_sequence,
        } => {
          self.model.resize(&terminal_size);
          self.control.acknowledge_geometry(observed_sequence).await?;
        }
        AttachmentEvent::ServerError { message: error, .. } => message = Some(error),
        AttachmentEvent::SessionEnded { .. } | AttachmentEvent::Exited { .. } => {
          self.connected = false;
        }
        _ => {}
      }
    }
    Ok(message)
  }

  pub async fn close(&mut self) {
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
    if let Some(runner) = &self.runner {
      runner.abort();
    }
  }
}
