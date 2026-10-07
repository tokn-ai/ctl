use ctmux_client::{
  AttachmentAcknowledgementError, AttachmentControl, AttachmentEvent, AttachmentEvents,
};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager as _;
use tauri::ipc::Channel;
use tokio::sync::{Mutex, Notify, watch};
use tokio::time::{sleep, timeout};

use crate::dto::{
  AttachmentEventDto, ConnectionTargetDto, PresentationAcknowledgement, observation_timestamp_ms,
};
use crate::error::{CommandErrorDto, CommandResult};

mod cache_writer;
mod observation;

const PRESENTATION_ACKNOWLEDGEMENT_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(30);
const ATTACHMENT_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Default)]
pub struct AppState {
  registry: Arc<Mutex<AttachmentRegistry>>,
  daemon_restart_transition: Arc<Mutex<()>>,
}

#[derive(Default)]
struct AttachmentRegistry {
  by_window: HashMap<String, HashMap<String, AttachmentSlot>>,
  window_transitions: HashMap<String, Arc<Mutex<()>>>,
}

enum AttachmentSlot {
  Opening {
    attachment_id: String,
    cancel: watch::Sender<bool>,
  },
  Active(Arc<AttachmentActor>),
}

pub struct AttachmentActor {
  pub attachment_id: String,
  pub window_label: String,
  pub target: ConnectionTargetDto,
  pub control: AttachmentControl,
  pub cache_identity: Option<ctmux_client::cache::CacheIdentity>,
  pub remote_observation: Option<crate::about::observations::RemoteObservation>,
  pending: Mutex<Option<PendingPresentation>>,
  pending_changed: Notify,
  closed: AtomicBool,
  closed_changed: Notify,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingPresentation {
  event_id: String,
  acknowledgement: PresentationAcknowledgement,
  acknowledging: bool,
}

impl PendingPresentation {
  fn claim(&mut self, event_id: &str) -> CommandResult<PresentationAcknowledgement> {
    if self.event_id != event_id {
      return Err(CommandErrorDto::new(
        "stale_presentation_event",
        "the renderer event is no longer current",
      ));
    }
    if self.acknowledging {
      return Err(CommandErrorDto::new(
        "presentation_acknowledgement_in_progress",
        "the renderer event acknowledgement is already in progress",
      ));
    }
    self.acknowledging = true;
    Ok(self.acknowledgement)
  }
}

impl AppState {
  pub async fn remote_actors(&self) -> Vec<Arc<AttachmentActor>> {
    self
      .registry
      .lock()
      .await
      .by_window
      .values()
      .flat_map(|slots| slots.values())
      .filter_map(|slot| match slot {
        AttachmentSlot::Active(actor)
          if !actor.closed.load(Ordering::Acquire) && actor.remote_observation.is_some() =>
        {
          Some(Arc::clone(actor))
        }
        _ => None,
      })
      .collect()
  }

  pub async fn remote_observations(&self) -> Vec<crate::about::observations::RemoteObservation> {
    self
      .registry
      .lock()
      .await
      .by_window
      .values()
      .flat_map(|slots| slots.values())
      .filter_map(|slot| match slot {
        AttachmentSlot::Active(actor) if !actor.closed.load(Ordering::Acquire) => {
          actor.remote_observation.clone()
        }
        _ => None,
      })
      .collect()
  }

  /// Serializes opens without replacing sibling pane attachments. A stalled
  /// open can be cancelled without waiting for the window transition lock.
  pub async fn open_attachment<T>(
    &self,
    window_label: &str,
    attachment_id: &str,
    on_opening: impl FnOnce() -> CommandResult<()>,
    open: impl Future<Output = CommandResult<T>>,
  ) -> CommandResult<T> {
    let transition = self.window_transition(window_label).await;
    let _transition_guard = transition.lock().await;
    let mut cancelled = self.reserve_window(window_label, attachment_id).await?;

    let result = async {
      // Publish the ID only after cancellation is registered. A frontend
      // cancellation that precedes this notification can then be replayed.
      on_opening()?;
      tokio::select! {
        biased;
        _ = cancelled.wait_for(|cancelled| *cancelled) => Err(CommandErrorDto::new(
          "attachment_cancelled",
          "Session attachment cancelled.",
        )),
        result = open => result,
      }
    }
    .await;
    if result.is_err() {
      self.release(window_label, attachment_id).await;
    }
    result
  }

  pub async fn cancel_opening(&self, window_label: &str, attachment_id: &str) {
    let registry = self.registry.lock().await;
    if let Some(AttachmentSlot::Opening {
      attachment_id: current,
      cancel,
    }) = registry
      .by_window
      .get(window_label)
      .and_then(|slots| slots.get(attachment_id))
      && current == attachment_id
    {
      let _ = cancel.send(true);
    }
  }

  /// Returns the process-wide lock which serializes destructive daemon
  /// restart attempts across every GUI window.
  #[must_use]
  pub fn daemon_restart_transition(&self) -> Arc<Mutex<()>> {
    Arc::clone(&self.daemon_restart_transition)
  }

  pub async fn window_transition(&self, window_label: &str) -> Arc<Mutex<()>> {
    let mut registry = self.registry.lock().await;
    Arc::clone(
      registry
        .window_transitions
        .entry(window_label.into())
        .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
  }

  /// Detaches every local pane in the window before restarting its daemon.
  /// Remote panes remain attached.
  pub async fn detach_active_local_window(&self, window_label: &str) -> CommandResult<()> {
    let actors = {
      let registry = self.registry.lock().await;
      let mut actors = Vec::new();
      for slot in registry
        .by_window
        .get(window_label)
        .into_iter()
        .flat_map(|slots| slots.values())
      {
        match slot {
          AttachmentSlot::Opening { .. } => {
            return Err(CommandErrorDto::new(
              "window_attachment_transition_in_progress",
              "another attachment transition is already in progress for this window",
            ));
          }
          AttachmentSlot::Active(actor) if actor.target.is_local() => {
            actors.push(Arc::clone(actor));
          }
          AttachmentSlot::Active(_) => {}
        }
      }
      actors
    };
    for actor in actors {
      actor.detach_and_wait().await?;
    }
    Ok(())
  }

  pub async fn detach_session(
    &self,
    window_label: &str,
    host_key: &str,
    session_id: &str,
  ) -> CommandResult<()> {
    let actors: Vec<_> = self
      .registry
      .lock()
      .await
      .by_window
      .get(window_label)
      .into_iter()
      .flat_map(|slots| slots.values())
      .filter_map(|slot| match slot {
        AttachmentSlot::Active(actor)
          if actor.cache_identity.as_ref().is_some_and(|identity| {
            identity.host_key == host_key && identity.session_id == session_id
          }) =>
        {
          Some(Arc::clone(actor))
        }
        _ => None,
      })
      .collect();
    for actor in actors {
      actor.detach_and_wait().await?;
    }
    Ok(())
  }

  async fn reserve_window(
    &self,
    window_label: &str,
    attachment_id: &str,
  ) -> CommandResult<watch::Receiver<bool>> {
    let mut registry = self.registry.lock().await;
    let slots = registry.by_window.entry(window_label.into()).or_default();
    if slots.contains_key(attachment_id) {
      return Err(CommandErrorDto::new(
        "attachment_already_registered",
        "this attachment ID is already registered in the window",
      ));
    }
    let (cancel, cancelled) = watch::channel(false);
    slots.insert(
      attachment_id.into(),
      AttachmentSlot::Opening {
        attachment_id: attachment_id.into(),
        cancel,
      },
    );
    Ok(cancelled)
  }

  pub async fn activate(
    &self,
    window_label: &str,
    attachment_id: &str,
    actor: Arc<AttachmentActor>,
  ) -> CommandResult<()> {
    let mut registry = self.registry.lock().await;
    let reservation_matches = matches!(
      registry.by_window.get(window_label).and_then(|slots| slots.get(attachment_id)),
      Some(AttachmentSlot::Opening { attachment_id: reserved, .. }) if reserved == attachment_id
    );
    if !reservation_matches {
      return Err(CommandErrorDto::new(
        "attachment_reservation_lost",
        "the attachment window reservation is no longer active",
      ));
    }
    registry
      .by_window
      .get_mut(window_label)
      .expect("reservation exists")
      .insert(attachment_id.into(), AttachmentSlot::Active(actor));
    Ok(())
  }

  pub async fn release(&self, window_label: &str, attachment_id: &str) {
    let mut registry = self.registry.lock().await;
    let should_remove = match registry
      .by_window
      .get(window_label)
      .and_then(|slots| slots.get(attachment_id))
    {
      Some(AttachmentSlot::Opening {
        attachment_id: current,
        ..
      }) => current == attachment_id,
      Some(AttachmentSlot::Active(actor)) => actor.attachment_id == attachment_id,
      None => false,
    };
    if should_remove && let Some(slots) = registry.by_window.get_mut(window_label) {
      slots.remove(attachment_id);
      if slots.is_empty() {
        registry.by_window.remove(window_label);
      }
    }
  }

  pub async fn actor(
    &self,
    window_label: &str,
    attachment_id: &str,
  ) -> CommandResult<Arc<AttachmentActor>> {
    let registry = self.registry.lock().await;
    let Some(AttachmentSlot::Active(actor)) = registry
      .by_window
      .get(window_label)
      .and_then(|slots| slots.get(attachment_id))
    else {
      return Err(CommandErrorDto::new(
        "attachment_not_found",
        "this window has no active attachment with the requested ID",
      ));
    };
    if actor.attachment_id != attachment_id || actor.window_label != window_label {
      return Err(CommandErrorDto::new(
        "attachment_not_owned",
        "the attachment does not belong to this window",
      ));
    }
    Ok(Arc::clone(actor))
  }

  async fn detach_window(&self, window_label: &str) {
    let slots = self
      .registry
      .lock()
      .await
      .by_window
      .remove(window_label)
      .unwrap_or_default();
    for slot in slots.into_values() {
      match slot {
        AttachmentSlot::Opening { cancel, .. } => {
          let _ = cancel.send(true);
        }
        AttachmentSlot::Active(actor) => {
          let _ignored = actor.control.detach().await;
        }
      }
    }
  }
}

pub fn register_main_window_cleanup(app: &tauri::App) {
  let Some(window) = app.get_webview_window("main") else {
    return;
  };
  let state = app.state::<AppState>().inner().clone();
  let window_label = window.label().to_owned();
  window.on_window_event(move |event| {
    if matches!(event, tauri::WindowEvent::Destroyed) {
      crate::ssh_auth::cancel_window(&window_label);
      let state = state.clone();
      let window_label = window_label.clone();
      tauri::async_runtime::spawn(async move {
        state.detach_window(&window_label).await;
      });
    }
  });
}

impl AttachmentActor {
  pub fn new(
    attachment_id: String,
    window_label: String,
    target: ConnectionTargetDto,
    control: AttachmentControl,
  ) -> Self {
    Self {
      attachment_id,
      window_label,
      target,
      control,
      cache_identity: None,
      remote_observation: None,
      pending: Mutex::new(None),
      pending_changed: Notify::new(),
      closed: AtomicBool::new(false),
      closed_changed: Notify::new(),
    }
  }

  pub fn with_cache(mut self, identity: ctmux_client::cache::CacheIdentity) -> Self {
    self.cache_identity = Some(identity);
    self
  }

  pub fn with_remote_observation(
    mut self,
    observation: Option<crate::about::observations::RemoteObservation>,
  ) -> Self {
    self.remote_observation = observation;
    self
  }

  pub async fn set_pending(
    &self,
    acknowledgement: PresentationAcknowledgement,
  ) -> CommandResult<String> {
    let mut pending = self.pending.lock().await;
    if pending.is_some() {
      return Err(CommandErrorDto::new(
        "presentation_already_pending",
        "a renderer event is already awaiting acknowledgement",
      ));
    }
    let event_id = uuid::Uuid::new_v4().to_string();
    *pending = Some(PendingPresentation {
      event_id: event_id.clone(),
      acknowledgement,
      acknowledging: false,
    });
    Ok(event_id)
  }

  pub async fn acknowledge(&self, event_id: &str) -> CommandResult<()> {
    let acknowledgement = {
      let mut pending = self.pending.lock().await;
      let Some(pending) = pending.as_mut() else {
        return Err(CommandErrorDto::new(
          "presentation_not_pending",
          "there is no renderer event awaiting acknowledgement",
        ));
      };
      pending.claim(event_id)?
    };

    let result = match acknowledgement.apply(&self.control).await {
      // The controller can finish after queuing final presentation/Ended
      // events. This exact pending event still proves local rendering; there
      // is simply no transport left to acknowledge. Rejected acknowledgements
      // continue to fail instead of accepting an invalid presentation order.
      Err(AttachmentAcknowledgementError::Closed) => Ok(()),
      result => result,
    };
    let mut pending = self.pending.lock().await;
    let still_current = pending
      .as_ref()
      .is_some_and(|pending| pending.event_id == event_id);
    if still_current {
      if result.is_ok() {
        *pending = None;
        self.pending_changed.notify_waiters();
      } else if let Some(pending) = pending.as_mut() {
        pending.acknowledging = false;
      }
    }
    result.map_err(|error| acknowledgement_error(&error))
  }

  pub async fn clear_pending(&self) {
    let mut pending = self.pending.lock().await;
    *pending = None;
    self.pending_changed.notify_waiters();
  }

  pub async fn wait_until_presentation_applied(&self) {
    loop {
      let notified = self.pending_changed.notified();
      if self.pending.lock().await.is_none() {
        return;
      }
      notified.await;
    }
  }

  pub async fn has_pending_presentation(&self) -> bool {
    self.pending.lock().await.is_some()
  }

  pub async fn wait_until_closed(&self) {
    loop {
      let notified = self.closed_changed.notified();
      if self.closed.load(Ordering::Acquire) {
        return;
      }
      notified.await;
    }
  }

  pub async fn detach_and_wait(&self) -> CommandResult<()> {
    if self.closed.load(Ordering::Acquire) {
      return Ok(());
    }

    let detach_error = self
      .control
      .detach()
      .await
      .err()
      .map(CommandErrorDto::backend);
    if timeout(ATTACHMENT_CLOSE_TIMEOUT, self.wait_until_closed())
      .await
      .is_ok()
    {
      return Ok(());
    }
    if let Some(error) = detach_error {
      return Err(error);
    }
    Err(CommandErrorDto::new(
      "attachment_detach_timeout",
      "the attachment did not close within five seconds",
    ))
  }

  fn mark_closed(&self) {
    self.closed.store(true, Ordering::Release);
    self.closed_changed.notify_waiters();
  }
}

fn acknowledgement_error(error: &AttachmentAcknowledgementError) -> CommandErrorDto {
  CommandErrorDto::new("presentation_acknowledgement_failed", error.to_string())
}

pub async fn forward_attachment_events(
  state: AppState,
  actor: Arc<AttachmentActor>,
  mut events: AttachmentEvents,
  channel: Channel<AttachmentEventDto>,
  controller: impl std::future::Future<
    Output = Result<ctmux_client::AttachExit, ctmux_client::ClientError>,
  >,
) {
  tokio::pin!(controller);

  let cache_writer = actor
    .cache_identity
    .clone()
    .map(|identity| cache_writer::CacheWriter::new(identity, actor.control.clone()));
  let mut observations = observation::Observations::default();
  let mut bridge_error = None;
  let outcome = loop {
    if actor.has_pending_presentation().await {
      tokio::select! {
        result = &mut controller => break result,
        () = actor.wait_until_presentation_applied() => {}
        () = sleep(PRESENTATION_ACKNOWLEDGEMENT_TIMEOUT) => {
          bridge_error = Some(CommandErrorDto::new(
            "presentation_acknowledgement_timeout",
            "the terminal renderer did not acknowledge its event within 30 seconds",
          ));
          let _ignored = actor.control.detach().await;
          break controller.await;
        }
      }
      continue;
    }

    tokio::select! {
      biased;
      event = events.recv() => {
        let Some(event) = event else {
          break controller.await;
        };
        if let Err(error) = observe_event(&mut observations, &actor.attachment_id, &channel, &event) {
          bridge_error = Some(error);
          let _ignored = actor.control.detach().await;
          break controller.await;
        }
        if let Some(cache_writer) = &cache_writer {
          cache_writer.enqueue(&event);
        }
        let forwarding = forward_event(&actor, &channel, event);
        tokio::pin!(forwarding);
        let forwarded = tokio::select! {
          result = &mut forwarding => result,
          outcome = &mut controller => {
            // Finish publishing the already received event before closure.
            if let Err(error) = forwarding.await {
              bridge_error = Some(error);
            }
            break outcome;
          }
        };
        if let Err(error) = forwarded {
          bridge_error = Some(error);
          let _ignored = actor.control.detach().await;
          break controller.await;
        }
      }
      result = &mut controller => break result,
    }
  };

  let mut require_checkpoint = actor.has_pending_presentation().await;
  if bridge_error.is_none() {
    // A completed controller has already queued all final frames. Keep the
    // actor available for their renderer acknowledgements and deliver Ended
    // before publishing closure. The remote resume cursor cannot account for
    // these locally completed presentations, so abnormal reconnects still
    // require a checkpoint.
    match forward_buffered_events(
      &actor,
      &mut events,
      &channel,
      cache_writer.as_ref(),
      &mut observations,
    )
    .await
    {
      Ok(rendered_after_close) => require_checkpoint |= rendered_after_close,
      Err(error) => bridge_error = Some(error),
    }
  }
  actor.clear_pending().await;
  if let Some(cache_writer) = cache_writer
    && let Err(error) = cache_writer.finish().await
  {
    let _ignored = channel.send(AttachmentEventDto::ServerError {
      attachment_id: actor.attachment_id.clone(),
      code: "local_cache_failed".into(),
      message: error.message,
    });
  }
  // A timeout/disconnect is not new contact. Flush only the last incoming time.
  let _ignored = publish_observation(&actor.attachment_id, &channel, observations.flush());
  publish_attachment_outcome(&actor, &channel, outcome, bridge_error, require_checkpoint);
  state
    .release(&actor.window_label, &actor.attachment_id)
    .await;
  actor.mark_closed();
}

fn publish_attachment_outcome(
  actor: &AttachmentActor,
  channel: &Channel<AttachmentEventDto>,
  outcome: Result<ctmux_client::AttachExit, ctmux_client::ClientError>,
  bridge_error: Option<CommandErrorDto>,
  require_checkpoint: bool,
) {
  if let Some(error) = bridge_error {
    let _ignored = channel.send(AttachmentEventDto::attachment_error(
      &actor.attachment_id,
      error.code,
      error.message,
    ));
  } else {
    match outcome {
      Ok(exit) => {
        let _ignored = channel.send(AttachmentEventDto::attachment_exited(
          &actor.attachment_id,
          &exit,
          require_checkpoint,
        ));
      }
      Err(error) => {
        let _ignored = channel.send(AttachmentEventDto::attachment_error(
          &actor.attachment_id,
          "attachment_failed",
          error.to_string(),
        ));
      }
    }
  }
}

async fn forward_buffered_events(
  actor: &AttachmentActor,
  events: &mut AttachmentEvents,
  channel: &Channel<AttachmentEventDto>,
  cache_writer: Option<&cache_writer::CacheWriter>,
  observations: &mut observation::Observations,
) -> CommandResult<bool> {
  let mut rendered_after_close = false;
  loop {
    if actor.has_pending_presentation().await {
      timeout(
        PRESENTATION_ACKNOWLEDGEMENT_TIMEOUT,
        actor.wait_until_presentation_applied(),
      )
      .await
      .map_err(|_| {
        CommandErrorDto::new(
          "presentation_acknowledgement_timeout",
          "the terminal renderer did not acknowledge its final event within 30 seconds",
        )
      })?;
    }
    let Ok(event) = events.try_recv() else {
      return Ok(rendered_after_close);
    };
    rendered_after_close |= matches!(
      event,
      AttachmentEvent::Checkpoint { .. }
        | AttachmentEvent::Output { .. }
        | AttachmentEvent::PtyGeometryChanged { .. }
    );
    observe_event(observations, &actor.attachment_id, channel, &event)?;
    if let Some(cache_writer) = cache_writer {
      cache_writer.enqueue(&event);
    }
    forward_event(actor, channel, event).await?;
  }
}

fn observe_event(
  observations: &mut observation::Observations,
  attachment_id: &str,
  channel: &Channel<AttachmentEventDto>,
  event: &AttachmentEvent,
) -> CommandResult<()> {
  // Capture receipt before filesystem writes or renderer acknowledgements can
  // delay forwarding. Idle heartbeats are observations too.
  let observed = observations.record(event, observation_timestamp_ms(), std::time::Instant::now());
  publish_observation(attachment_id, channel, observed)
}

fn publish_observation(
  attachment_id: &str,
  channel: &Channel<AttachmentEventDto>,
  observed_at_ms: Option<u64>,
) -> CommandResult<()> {
  if let Some(last_seen_at_ms) = observed_at_ms {
    channel
      .send(AttachmentEventDto::SessionObserved {
        attachment_id: attachment_id.to_owned(),
        last_seen_at_ms,
      })
      .map_err(|error| CommandErrorDto::new("attachment_channel_closed", error.to_string()))?;
  }
  Ok(())
}

async fn forward_event(
  actor: &AttachmentActor,
  channel: &Channel<AttachmentEventDto>,
  event: AttachmentEvent,
) -> CommandResult<()> {
  let event = match event {
    AttachmentEvent::Checkpoint {
      checkpoint,
      history,
      history_manifest,
      history_gap,
    } => {
      let acknowledgement = PresentationAcknowledgement::Checkpoint {
        sequence: checkpoint.sequence,
      };
      let event_id = actor.set_pending(acknowledgement).await?;
      AttachmentEventDto::checkpoint(
        &actor.attachment_id,
        event_id,
        checkpoint,
        history,
        history_manifest,
        history_gap,
      )
    }
    AttachmentEvent::HistorySynced {
      snapshot_id,
      checkpoint,
      history,
      rows,
      scrollback_limit,
      history_gap,
    } => AttachmentEventDto::HistorySynced {
      attachment_id: actor.attachment_id.clone(),
      snapshot_id,
      checkpoint: checkpoint.into(),
      history: history.into(),
      rows,
      scrollback_limit: scrollback_limit.to_string(),
      history_gap,
    },
    AttachmentEvent::Output {
      sequence_start,
      sequence_end,
      data,
    } => {
      let acknowledgement = PresentationAcknowledgement::Output { sequence_end };
      let event_id = actor.set_pending(acknowledgement).await?;
      AttachmentEventDto::output(
        &actor.attachment_id,
        event_id,
        sequence_start,
        sequence_end,
        &data,
      )
    }
    AttachmentEvent::PtyGeometryChanged {
      terminal_size,
      observed_sequence,
    } => {
      let acknowledgement = PresentationAcknowledgement::Geometry { observed_sequence };
      let event_id = actor.set_pending(acknowledgement).await?;
      AttachmentEventDto::pty_geometry_changed(
        &actor.attachment_id,
        event_id,
        terminal_size,
        observed_sequence,
      )
    }
    AttachmentEvent::PaneResizeResult {
      request_id,
      outcome,
    } => AttachmentEventDto::pane_resize_result(&actor.attachment_id, request_id, outcome),
    AttachmentEvent::ViewChanged { view } => AttachmentEventDto::ViewChanged {
      attachment_id: actor.attachment_id.clone(),
      view: view.into(),
    },
    AttachmentEvent::LeaseStatus {
      lease,
      status,
      notification,
    } => AttachmentEventDto::lease_status(&actor.attachment_id, lease, status, notification),
    AttachmentEvent::ShellStateChanged { state } => {
      AttachmentEventDto::shell_state_changed(&actor.attachment_id, state)
    }
    AttachmentEvent::ServerError { code, message } => {
      AttachmentEventDto::server_error(&actor.attachment_id, &code, message)
    }
    AttachmentEvent::SessionEnded {
      session_id,
      exit_code,
    } => AttachmentEventDto::session_ended(&actor.attachment_id, session_id, exit_code),
    AttachmentEvent::HeartbeatAck { .. } | AttachmentEvent::Exited { .. } => return Ok(()),
  };

  channel
    .send(event)
    .map_err(|error| CommandErrorDto::new("attachment_channel_closed", error.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;

  async fn buffered_test_attachment() -> (
    tokio::io::DuplexStream,
    tokio::io::DuplexStream,
    ctmux_client::AttachedSession,
  ) {
    use ctmux_client::{
      AttachRequest, ClientIdentity, DEFAULT_PRESENTATION_WINDOW_BYTES, begin_attach,
    };
    use ctmux_proto::{
      ClientMessage, LeaseStatus, ServerMessage, ShellState, TerminalSize, read_frame, write_frame,
    };
    let (client, mut peer) = tokio::io::duplex(16 * 1024);
    let server = async {
      assert!(matches!(
        read_frame::<_, ClientMessage>(&mut peer).await.unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      write_frame(
        &mut peer,
        &ServerMessage::HandshakeAccepted {
          protocol_version: ctmux_proto::PROTOCOL_VERSION,
          protocols: vec![ctmux_proto::protocol_info()],
          server_version: "test".into(),
          build: None,
          heartbeat_interval_ms: 60_000,
          attachment_liveness_timeout_ms: 180_000,
        },
      )
      .await
      .unwrap();
      assert!(matches!(
        read_frame::<_, ClientMessage>(&mut peer).await.unwrap(),
        Some(ClientMessage::AttachSession { .. })
      ));
      write_frame(
        &mut peer,
        &ServerMessage::Attached {
          attachment_token: "token".into(),
          session: ctmux_proto::SessionInfo {
            session_id: "session".into(),
            terminal_id: "terminal".into(),
            view_id: "view".into(),
            name: "test".into(),
            status: ctmux_proto::SessionStatus::Running,
            created_at_ms: 0,
            next_sequence: 0,
            terminal_size: TerminalSize::default(),
          },
          earliest_sequence: 0,
          next_sequence: 0,
          replay_from: 0,
          history_gap: false,
          history_manifest: None,
          checkpoint: None,
          history: None,
          terminal_size_mismatch: false,
          input_lease: LeaseStatus {
            held: false,
            owned_by_client: false,
          },
          layout_lease: LeaseStatus {
            held: false,
            owned_by_client: false,
          },
          shell_state: ShellState::default(),
        },
      )
      .await
      .unwrap();
      peer
    };
    let identity = ClientIdentity {
      name: "desktop-test".into(),
      version: "test".into(),
    };
    let opening = begin_attach(
      client,
      &identity,
      AttachRequest {
        session: "session".into(),
        resume_from: Some(0),
        terminal_size: TerminalSize::default(),
        request_input_lease: false,
        request_layout_lease: false,
        request_command_line: false,
        request_running_command: false,
        presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
      },
    );
    let (opened, peer) = tokio::join!(opening, server);
    let (client, attached) = opened.unwrap();
    (client, peer, attached)
  }

  #[tokio::test]
  async fn completed_controller_drains_final_frames_before_attachment_closure() {
    use ctmux_client::{AttachmentController, AttachmentControllerOptions};
    use ctmux_proto::{ServerMessage, write_frame};
    for natural_exit in [true, false] {
      let (client, mut peer, attached) = buffered_test_attachment().await;
      for (sequence_start, data) in [(0, b"last".to_vec()), (4, b" tail".to_vec())] {
        write_frame(
          &mut peer,
          &ServerMessage::Output {
            sequence_start,
            sequence_end: sequence_start + data.len() as u64,
            data,
          },
        )
        .await
        .unwrap();
      }
      if natural_exit {
        write_frame(
          &mut peer,
          &ServerMessage::SessionEnded {
            session_id: "session".into(),
            exit_code: Some(7),
          },
        )
        .await
        .unwrap();
      }
      drop(peer);
      let (controller, control, events) =
        AttachmentController::new(client, &attached, AttachmentControllerOptions::default())
          .unwrap();
      // Complete the transport before any frontend frame is acknowledged.
      let exit = controller.run().await.unwrap();
      let actor = Arc::new(AttachmentActor::new(
        "owner".into(),
        "main".into(),
        ConnectionTargetDto::Local,
        control,
      ));
      let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
      let channel = Channel::new(move |body| {
        sender
          .send(body.deserialize::<serde_json::Value>().unwrap())
          .unwrap();
        Ok(())
      });
      let renderer_actor = Arc::clone(&actor);
      let rendering = async move {
        let mut delivered = Vec::new();
        while let Some(event) = receiver.recv().await {
          if let Some(event_id) = event["event_id"].as_str() {
            let stale = renderer_actor.acknowledge("wrong-event").await.unwrap_err();
            assert_eq!(stale.code, "stale_presentation_event");
            renderer_actor.acknowledge(event_id).await.unwrap();
          }
          delivered.push(event);
        }
        delivered
      };
      let forwarding = forward_attachment_events(
        AppState::default(),
        Arc::clone(&actor),
        events,
        channel,
        async { Ok(exit) },
      );
      let ((), delivered) = timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(forwarding, rendering)
      })
      .await
      .expect("final renderer frames did not drain");
      let types: Vec<_> = delivered
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect();
      assert_eq!(types.iter().filter(|kind| **kind == "output").count(), 2);
      assert!(!types.contains(&"attachment_error"));
      if natural_exit {
        assert!(
          types
            .iter()
            .position(|kind| *kind == "session_ended")
            .unwrap()
            < types
              .iter()
              .position(|kind| *kind == "attachment_exited")
              .unwrap()
        );
        assert_eq!(delivered.last().unwrap()["reason"], "session_ended");
      } else {
        assert_eq!(delivered.last().unwrap()["reason"], "connection_closed");
        assert!(delivered.last().unwrap()["next_sequence"].is_null());
      }
      assert!(!actor.has_pending_presentation().await);
      assert!(actor.closed.load(Ordering::Acquire));
    }
  }

  #[tokio::test]
  async fn pane_resize_results_keep_operation_ids_and_errors_without_presentation_acks() {
    let (client, _peer, attached) = buffered_test_attachment().await;
    let (_controller, control, _events) = ctmux_client::AttachmentController::new(
      client,
      &attached,
      ctmux_client::AttachmentControllerOptions::default(),
    )
    .unwrap();
    let actor = AttachmentActor::new(
      "owner".into(),
      "main".into(),
      ConnectionTargetDto::Local,
      control,
    );
    let (sender, receiver) = std::sync::mpsc::channel();
    let channel = Channel::new(move |body| {
      sender
        .send(body.deserialize::<serde_json::Value>().unwrap())
        .unwrap();
      Ok(())
    });
    forward_event(
      &actor,
      &channel,
      AttachmentEvent::PaneResizeResult {
        request_id: "rejected-operation".into(),
        outcome: ctmux_proto::PaneResizeOutcome::Rejected {
          code: ctmux_proto::ErrorCode::LayoutLeaseRequired,
          message: "Take resize control.".into(),
        },
      },
    )
    .await
    .unwrap();
    let rejected = receiver.try_recv().unwrap();
    assert_eq!(rejected["event_type"], "pane_resize_result");
    assert_eq!(rejected["request_id"], "rejected-operation");
    assert_eq!(rejected["error"]["code"], "layout_lease_required");
    assert!(rejected["view"].is_null());
    let view = ctmux_proto::ViewInfo {
      session_id: "session".into(),
      session_name: "shell".into(),
      view_id: "view".into(),
      revision: u64::MAX,
      canvas_size: ctmux_proto::TerminalSize::default(),
      zoomed_terminal_id: None,
      panes: vec![],
      terminals: vec![],
      layout: ctmux_proto::ViewLayout::Split {
        axis: ctmux_proto::SplitAxis::Horizontal,
        weights: vec![2, 1],
        children: ["first", "second"]
          .map(|id| ctmux_proto::ViewLayout::Terminal {
            terminal_id: id.into(),
          })
          .into(),
      },
    };
    forward_event(
      &actor,
      &channel,
      AttachmentEvent::PaneResizeResult {
        request_id: "applied-operation".into(),
        outcome: ctmux_proto::PaneResizeOutcome::Applied {
          view: Box::new(view),
        },
      },
    )
    .await
    .unwrap();
    let applied = receiver.try_recv().unwrap();
    assert_eq!(applied["request_id"], "applied-operation");
    assert_eq!(applied["view"]["revision"], u64::MAX.to_string());
    assert_eq!(
      applied["view"]["layout"]["weights"],
      serde_json::json!([2, 1])
    );
    assert!(applied["error"].is_null());
    assert!(!actor.has_pending_presentation().await);
  }

  #[test]
  fn quiet_heartbeat_publishes_an_observation_without_a_presentation_event() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let channel = Channel::new(move |body| {
      sender
        .send(body.deserialize::<serde_json::Value>().unwrap())
        .unwrap();
      Ok(())
    });
    let mut observations = observation::Observations::default();
    observe_event(
      &mut observations,
      "quiet-attachment",
      &channel,
      &AttachmentEvent::HeartbeatAck { nonce: 1 },
    )
    .unwrap();
    let event = receiver.try_recv().unwrap();
    assert_eq!(event["event_type"], "session_observed");
    assert_eq!(event["attachment_id"], "quiet-attachment");
    assert!(
      event["last_seen_at_ms"]
        .as_u64()
        .is_some_and(crate::dto::valid_observation_timestamp)
    );
    assert!(receiver.try_recv().is_err());
  }

  #[tokio::test]
  async fn opening_and_releasing_a_pane_preserves_sibling_reservations() {
    let state = AppState::default();
    let first = state.reserve_window("main", "first").await.unwrap();
    state
      .open_attachment("main", "second", || Ok(()), async { Ok(()) })
      .await
      .unwrap();
    assert!(!*first.borrow());
    assert_eq!(state.registry.lock().await.by_window["main"].len(), 2);
    state.release("main", "second").await;
    assert!(!*first.borrow());
    assert_eq!(state.registry.lock().await.by_window["main"].len(), 1);
    state.cancel_opening("main", "first").await;
    assert!(*first.borrow());
  }

  #[tokio::test]
  async fn closing_a_window_cancels_all_its_panes_only() {
    let state = AppState::default();
    let first = state.reserve_window("main", "first").await.unwrap();
    let second = state.reserve_window("main", "second").await.unwrap();
    let other = state.reserve_window("other", "first").await.unwrap();
    state.detach_window("main").await;
    assert!(*first.borrow());
    assert!(*second.borrow());
    assert!(!*other.borrow());
    assert!(!state.registry.lock().await.by_window.contains_key("main"));
  }

  #[tokio::test]
  async fn cancelling_a_stalled_open_drops_its_work_and_releases_the_window() {
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
      fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
      }
    }

    let state = AppState::default();
    let opening_state = state.clone();
    let dropped = Arc::new(AtomicBool::new(false));
    let drop_flag = DropFlag(Arc::clone(&dropped));
    let (started, ready) = tokio::sync::oneshot::channel();
    let opening = tokio::spawn(async move {
      opening_state
        .open_attachment(
          "main",
          "stalled",
          || {
            let _ = started.send(());
            Ok(())
          },
          async move {
            let _drop_flag = drop_flag;
            std::future::pending::<CommandResult<()>>().await
          },
        )
        .await
    });
    ready.await.unwrap();
    state.cancel_opening("main", "stalled").await;
    let error = timeout(std::time::Duration::from_secs(1), opening)
      .await
      .unwrap()
      .unwrap()
      .unwrap_err();
    assert_eq!(error.code, "attachment_cancelled");
    assert!(dropped.load(Ordering::SeqCst));

    let next = state.open_attachment("main", "next", || Ok(()), async {
      Err::<(), _>(CommandErrorDto::new("next_open_ran", "next open ran"))
    });
    assert_eq!(
      timeout(std::time::Duration::from_secs(1), next)
        .await
        .unwrap()
        .unwrap_err()
        .code,
      "next_open_ran",
    );
  }

  #[tokio::test]
  async fn opening_cancellation_is_scoped_to_the_window_and_current_id() {
    let state = AppState::default();
    let cancelled = state.reserve_window("main", "current").await.unwrap();
    state.cancel_opening("secondary", "current").await;
    state.cancel_opening("main", "old").await;
    assert!(!*cancelled.borrow());
    state.cancel_opening("main", "current").await;
    assert!(*cancelled.borrow());
    state.release("main", "current").await;
    let replacement = state.reserve_window("main", "replacement").await.unwrap();
    state.cancel_opening("main", "current").await;
    assert!(!*replacement.borrow());
  }

  #[tokio::test]
  async fn window_cleanup_cancels_its_pending_open() {
    let state = AppState::default();
    let cancelled = state.reserve_window("main", "opening").await.unwrap();
    state.detach_window("main").await;
    assert!(*cancelled.borrow());
    assert!(state.reserve_window("main", "next").await.is_ok());
  }

  #[tokio::test]
  async fn window_transitions_are_stable_and_window_scoped() {
    let state = AppState::default();
    let first = state.window_transition("main").await;
    let same_window = state.window_transition("main").await;
    let other_window = state.window_transition("secondary").await;

    assert!(Arc::ptr_eq(&first, &same_window));
    assert!(!Arc::ptr_eq(&first, &other_window));

    let _first_guard = first.lock().await;
    assert!(same_window.try_lock().is_err());
    assert!(other_window.try_lock().is_ok());
  }

  #[tokio::test]
  async fn daemon_restart_transition_is_shared_by_state_clones() {
    let state = AppState::default();
    let state_clone = state.clone();
    let first = state.daemon_restart_transition();
    let second = state_clone.daemon_restart_transition();

    assert!(Arc::ptr_eq(&first, &second));
    let _first_guard = first.lock().await;
    assert!(second.try_lock().is_err());
  }

  #[test]
  fn attachment_slot_tracks_the_reserved_id() {
    let slot = AttachmentSlot::Opening {
      attachment_id: "expected".into(),
      cancel: watch::channel(false).0,
    };
    assert!(matches!(
      slot,
      AttachmentSlot::Opening { attachment_id, .. } if attachment_id == "expected"
    ));
  }

  #[test]
  fn pending_presentation_rejects_a_stale_or_duplicate_event_id() {
    let mut pending = PendingPresentation {
      event_id: "current".into(),
      acknowledgement: PresentationAcknowledgement::Output { sequence_end: 9 },
      acknowledging: false,
    };

    let stale = pending.claim("old").unwrap_err();
    assert_eq!(stale.code, "stale_presentation_event");
    assert_eq!(
      pending.claim("current").unwrap(),
      PresentationAcknowledgement::Output { sequence_end: 9 }
    );
    let duplicate = pending.claim("current").unwrap_err();
    assert_eq!(duplicate.code, "presentation_acknowledgement_in_progress");
  }
}
