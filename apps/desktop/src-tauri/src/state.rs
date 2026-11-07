use ctmux_client::{
  AttachmentAcknowledgementError, AttachmentCommandError, AttachmentControl, AttachmentEvent,
  AttachmentEvents,
};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as RegistryMutex};
use tauri::ipc::Channel;
use tokio::sync::{Mutex, Notify, watch};
use tokio::time::timeout;

use crate::dto::{
  AttachmentEventDto, ConnectionTargetDto, PresentationAcknowledgement, observation_timestamp_ms,
};
use crate::error::{CommandErrorDto, CommandResult};

mod cache_writer;
mod document;
mod observation;
mod recovery;
mod statistics;

use recovery::AttachmentResume;

const PRESENTATION_ACKNOWLEDGEMENT_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(30);
const ATTACHMENT_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
type AttachmentOutcome = Result<ctmux_client::AttachExit, ctmux_client::ClientError>;

#[derive(Clone, Default)]
pub struct AppState {
  registry: Arc<RegistryMutex<AttachmentRegistry>>,
  daemon_restart_transition: Arc<Mutex<()>>,
}

#[derive(Default)]
struct AttachmentRegistry {
  by_window: HashMap<String, HashMap<String, AttachmentSlot>>,
  window_transitions: HashMap<String, Arc<Mutex<()>>>,
  documents: HashMap<String, Arc<document::WindowDocument>>,
  exiting: bool,
}

enum AttachmentSlot {
  Opening {
    attachment_id: String,
    cancel: watch::Sender<bool>,
  },
  Active(Arc<AttachmentActor>),
  Resumable(Arc<AttachmentResume>),
}

pub struct AttachmentActor {
  pub attachment_id: String,
  pub window_label: String,
  pub target: ConnectionTargetDto,
  pub control: AttachmentControl,
  pub cache_identity: Option<ctmux_client::cache::CacheIdentity>,
  pub remote_observation: Option<crate::about::observations::RemoteObservation>,
  resume: Option<Arc<AttachmentResume>>,
  pending: Mutex<Option<PendingPresentation>>,
  pending_changed: Notify,
  detaching: AtomicBool,
  renderer_retired: watch::Sender<bool>,
  abort_bridge: Notify,
  closed: AtomicBool,
  closed_changed: Notify,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingPresentation {
  event_id: String,
  acknowledgement: PresentationAcknowledgement,
  acknowledging: bool,
  started_at: tokio::time::Instant,
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
  pub fn remote_actors(&self) -> Vec<Arc<AttachmentActor>> {
    self
      .registry
      .lock()
      .unwrap()
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

  pub fn remote_observations(&self) -> Vec<crate::about::observations::RemoteObservation> {
    self
      .registry
      .lock()
      .unwrap()
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
    let document = self.current_document(window_label)?;
    document.wait_for_previous().await;
    let transition = self.window_transition(window_label);
    let _transition_guard = transition.lock().await;
    let mut cancelled = self.reserve_document(window_label, &document, attachment_id)?;

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
      self.release(window_label, attachment_id);
    }
    result
  }

  pub fn cancel_opening(&self, window_label: &str, attachment_id: &str) {
    let registry = self.registry.lock().unwrap();
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

  pub fn window_transition(&self, window_label: &str) -> Arc<Mutex<()>> {
    let mut registry = self.registry.lock().unwrap();
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
      let registry = self.registry.lock().unwrap();
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
          AttachmentSlot::Active(_) | AttachmentSlot::Resumable(_) => {}
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
      .unwrap()
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

  fn reserve_document(
    &self,
    window_label: &str,
    document: &Arc<document::WindowDocument>,
    attachment_id: &str,
  ) -> CommandResult<watch::Receiver<bool>> {
    let mut registry = self.registry.lock().unwrap();
    if !registry.is_current_document(window_label, document) {
      return Err(document::stale_document());
    }
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

  #[cfg(test)]
  fn reserve_window(
    &self,
    window_label: &str,
    attachment_id: &str,
  ) -> CommandResult<watch::Receiver<bool>> {
    let document = self.current_document(window_label)?;
    self.reserve_document(window_label, &document, attachment_id)
  }

  pub fn activate(
    &self,
    window_label: &str,
    attachment_id: &str,
    actor: Arc<AttachmentActor>,
    resume_attachment_id: Option<&str>,
  ) -> CommandResult<()> {
    let mut registry = self.registry.lock().unwrap();
    let Some(slots) = registry.by_window.get_mut(window_label) else {
      return Err(CommandErrorDto::new(
        "attachment_reservation_lost",
        "the attachment window reservation is no longer active",
      ));
    };
    match slots.get(attachment_id) {
      Some(AttachmentSlot::Opening { cancel, .. }) if *cancel.borrow() => {
        return Err(CommandErrorDto::new(
          "attachment_cancelled",
          "Session attachment cancelled.",
        ));
      }
      Some(AttachmentSlot::Opening { .. }) => {}
      _ => {
        return Err(CommandErrorDto::new(
          "attachment_reservation_lost",
          "the attachment window reservation is no longer active",
        ));
      }
    }
    slots.insert(attachment_id.into(), AttachmentSlot::Active(actor));
    if let Some(previous_id) = resume_attachment_id {
      recovery::remove_resume(slots, previous_id);
    }
    Ok(())
  }

  pub fn release(&self, window_label: &str, attachment_id: &str) {
    let mut registry = self.registry.lock().unwrap();
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
      Some(AttachmentSlot::Resumable(_)) | None => false,
    };
    if should_remove && let Some(slots) = registry.by_window.get_mut(window_label) {
      slots.remove(attachment_id);
      if slots.is_empty() {
        registry.by_window.remove(window_label);
      }
    }
  }

  pub fn actor(
    &self,
    window_label: &str,
    attachment_id: &str,
  ) -> CommandResult<Arc<AttachmentActor>> {
    let registry = self.registry.lock().unwrap();
    let actor = match registry
      .by_window
      .get(window_label)
      .and_then(|slots| slots.get(attachment_id))
    {
      Some(AttachmentSlot::Active(actor)) => actor,
      Some(AttachmentSlot::Resumable(_)) => {
        return Err(CommandErrorDto::attachment_command(
          AttachmentCommandError::Closed,
        ));
      }
      _ => {
        return Err(CommandErrorDto::new(
          "attachment_not_found",
          "this window has no active attachment with the requested ID",
        ));
      }
    };
    if actor.attachment_id != attachment_id || actor.window_label != window_label {
      return Err(CommandErrorDto::new(
        "attachment_not_owned",
        "the attachment does not belong to this window",
      ));
    }
    Ok(Arc::clone(actor))
  }
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
      resume: None,
      pending: Mutex::new(None),
      pending_changed: Notify::new(),
      detaching: AtomicBool::new(false),
      renderer_retired: watch::channel(false).0,
      abort_bridge: Notify::new(),
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
      started_at: tokio::time::Instant::now(),
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
    self.detaching.store(true, Ordering::Release);
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

async fn detach_controller(
  control: &AttachmentControl,
  controller: impl Future<Output = AttachmentOutcome>,
) -> AttachmentOutcome {
  tokio::pin!(controller);
  // The bounded command queue may be full, including before run() is first
  // polled. Keep its consumer running while requesting cleanup.
  tokio::select! {
    result = &mut controller => result,
    _ = control.detach() => controller.await,
  }
}

pub async fn forward_attachment_events(
  state: AppState,
  actor: Arc<AttachmentActor>,
  events: AttachmentEvents,
  channel: Channel<AttachmentEventDto>,
  controller: impl Future<Output = AttachmentOutcome>,
) {
  tokio::select! {
    biased;
    () = actor.abort_bridge.notified() => {}
    () = drive_attachment_bridge(&state, &actor, events, channel, controller) => {}
  }
  actor.mark_closed();
}

async fn drive_attachment_bridge(
  state: &AppState,
  actor: &Arc<AttachmentActor>,
  mut events: AttachmentEvents,
  channel: Channel<AttachmentEventDto>,
  controller: impl Future<Output = AttachmentOutcome>,
) {
  tokio::pin!(controller);

  let cache_writer = actor
    .cache_identity
    .clone()
    .map(|identity| cache_writer::CacheWriter::new(identity, actor.control.clone()));
  let mut observations = observation::Observations::default();
  let mut bridge_error = None;
  let mut statistics_interval = statistics::sampling_interval();
  let mut retired = actor.renderer_retired.subscribe();
  let outcome = loop {
    if *retired.borrow() {
      break drain_without_renderer(&mut events, cache_writer.as_ref(), &mut controller).await;
    }
    if actor.has_pending_presentation().await {
      tokio::select! {
        _ = retired.changed() => {},
        result = &mut controller => break result,
        result = statistics::wait_for_presentation(actor, &channel, &mut statistics_interval, false) => {
          if let Err(error) = result {
            bridge_error = Some(error);
            break detach_controller(&actor.control, &mut controller).await;
          }
        }
      }
      continue;
    }

    tokio::select! {
      biased;
      _ = retired.changed() => {}
      _ = statistics_interval.tick() => {
        if let Err(error) = statistics::publish(actor, &channel).await {
          bridge_error = Some(error);
          break detach_controller(&actor.control, &mut controller).await;
        }
      }
      event = events.recv() => {
        let Some(event) = event else {
          break controller.await;
        };
        if let Err(error) = observe_event(&mut observations, &actor.attachment_id, &channel, &event) {
          bridge_error = Some(error);
          break detach_controller(&actor.control, &mut controller).await;
        }
        if let Some(cache_writer) = &cache_writer {
          cache_writer.enqueue(&event);
        }
        let forwarding = forward_event(actor, &channel, event);
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
          break detach_controller(&actor.control, &mut controller).await;
        }
      }
      result = &mut controller => break result,
    }
  };

  let mut require_checkpoint = actor.has_pending_presentation().await;
  if !*retired.borrow() && bridge_error.is_none() {
    // A completed controller has already queued all final frames. Keep the
    // actor available for their renderer acknowledgements and deliver Ended
    // before publishing closure. The remote resume cursor cannot account for
    // these locally completed presentations, so abnormal reconnects still
    // require a checkpoint.
    let draining = forward_buffered_events(
      actor,
      &mut events,
      &channel,
      cache_writer.as_ref(),
      &mut observations,
      &mut statistics_interval,
    );
    tokio::select! {
      _ = retired.changed() => {},
      result = draining => match result {
        Ok(rendered_after_close) => require_checkpoint |= rendered_after_close,
        Err(error) => bridge_error = Some(error),
      }
    }
  }
  actor.clear_pending().await;
  finish_attachment_cache(actor, &channel, cache_writer).await;
  if *retired.borrow() {
    return;
  }
  // A timeout/disconnect is not new contact. Flush only the last incoming time.
  let _ignored = publish_observation(&actor.attachment_id, &channel, observations.flush());
  let _ignored = statistics::publish(actor, &channel).await;
  finish_attachment_bridge(
    state,
    actor,
    &channel,
    outcome,
    bridge_error,
    require_checkpoint,
  );
}

async fn drain_without_renderer(
  events: &mut AttachmentEvents,
  cache_writer: Option<&cache_writer::CacheWriter>,
  controller: impl Future<Output = AttachmentOutcome>,
) -> AttachmentOutcome {
  tokio::pin!(controller);
  // Consume queued output while waiting for the peer's Detach reply.
  loop {
    tokio::select! {
      result = &mut controller => return result,
      event = events.recv() => match event {
        Some(event) => {
          if let Some(cache_writer) = cache_writer {
            cache_writer.enqueue(&event);
          }
        }
        None => return controller.await,
      }
    }
  }
}

async fn finish_attachment_cache(
  actor: &AttachmentActor,
  channel: &Channel<AttachmentEventDto>,
  cache_writer: Option<cache_writer::CacheWriter>,
) {
  if let Some(cache_writer) = cache_writer
    && let Err(error) = cache_writer.finish().await
    && !*actor.renderer_retired.borrow()
  {
    let _ignored = channel.send(AttachmentEventDto::ServerError {
      attachment_id: actor.attachment_id.clone(),
      code: "local_cache_failed".into(),
      message: error.message,
    });
  }
}

fn finish_attachment_bridge(
  state: &AppState,
  actor: &Arc<AttachmentActor>,
  channel: &Channel<AttachmentEventDto>,
  outcome: AttachmentOutcome,
  bridge_error: Option<CommandErrorDto>,
  require_checkpoint: bool,
) {
  let disconnected = bridge_error.is_none()
    && matches!(
      &outcome,
      Ok(exit) if exit.reason == ctmux_client::AttachExitReason::ConnectionClosed
    );
  // The renderer can open the replacement as soon as it receives closure.
  if disconnected {
    state.finish_attachment(actor, true);
  }
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
  if !disconnected {
    state.finish_attachment(actor, false);
  }
}

async fn forward_buffered_events(
  actor: &AttachmentActor,
  events: &mut AttachmentEvents,
  channel: &Channel<AttachmentEventDto>,
  cache_writer: Option<&cache_writer::CacheWriter>,
  observations: &mut observation::Observations,
  statistics_interval: &mut tokio::time::Interval,
) -> CommandResult<bool> {
  let mut rendered_after_close = false;
  loop {
    if actor.has_pending_presentation().await {
      statistics::wait_for_presentation(actor, channel, statistics_interval, true).await?;
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
    AttachmentEvent::PaneMoveResult { .. }
    | AttachmentEvent::HeartbeatAck { .. }
    | AttachmentEvent::Exited { .. } => return Ok(()),
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

  #[tokio::test(start_paused = true)]
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
      let actor = Arc::new(
        AttachmentActor::new(
          "owner".into(),
          "main".into(),
          ConnectionTargetDto::Local,
          control,
        )
        .with_resume(&attached),
      );
      let state = AppState::default();
      state.reserve_window("main", "owner").unwrap();
      state
        .activate("main", "owner", Arc::clone(&actor), None)
        .unwrap();
      let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
      let channel = Channel::new(move |body| {
        sender
          .send(body.deserialize::<serde_json::Value>().unwrap())
          .unwrap();
        Ok(())
      });
      let renderer_actor = Arc::clone(&actor);
      let renderer_state = state.clone();
      let rendering = async move {
        let mut delivered = Vec::new();
        let mut delayed = false;
        while let Some(event) = receiver.recv().await {
          if let Some(event_id) = event["event_id"].as_str() {
            let wrong_event_error = renderer_actor.acknowledge("wrong-event").await.unwrap_err();
            assert_eq!(wrong_event_error.code, "stale_presentation_event");
            if !delayed {
              // The transport is already closed, but final renderer progress
              // must remain visible through statistics during the drain.
              tokio::time::sleep(std::time::Duration::from_millis(600)).await;
              delayed = true;
            }
            renderer_actor.acknowledge(event_id).await.unwrap();
          }
          if event["event_type"] == "attachment_exited" {
            let resume = renderer_state.resume_attachment(
              "main",
              "owner",
              &ConnectionTargetDto::Local,
              "terminal",
            );
            assert_eq!(resume.is_some(), !natural_exit);
          }
          delivered.push(event);
        }
        delivered
      };
      let forwarding =
        forward_attachment_events(state, Arc::clone(&actor), events, channel, async {
          Ok(exit)
        });
      let ((), delivered) = timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(forwarding, rendering)
      })
      .await
      .expect("final renderer frames did not drain");
      assert_final_delivery(&delivered, natural_exit);
      assert!(!actor.has_pending_presentation().await);
      assert!(actor.closed.load(Ordering::Acquire));
    }
  }

  fn assert_final_delivery(delivered: &[serde_json::Value], natural_exit: bool) {
    let types: Vec<_> = delivered
      .iter()
      .map(|event| event["event_type"].as_str().unwrap())
      .collect();
    assert_eq!(types.iter().filter(|kind| **kind == "output").count(), 2);
    assert!(delivered.iter().any(|event| {
      event["event_type"] == "attachment_statistics"
        && event["pending_events"] == 1
        && event["event_lag_ms"]
          .as_f64()
          .is_some_and(|lag| lag >= 500.0)
    }));
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
  }

  #[tokio::test(start_paused = true)]
  async fn a_closed_statistics_channel_cannot_deadlock_a_full_command_queue() {
    use ctmux_client::{AttachmentController, AttachmentControllerOptions};
    use ctmux_proto::{ClientMessage, LeaseKind, ServerMessage, read_frame, write_frame};

    let (client, mut peer, attached) = buffered_test_attachment().await;
    let (controller, control, events) = AttachmentController::new(
      client,
      &attached,
      AttachmentControllerOptions {
        command_queue_capacity: 1,
        ..Default::default()
      },
    )
    .unwrap();
    // Fill the queue before controller.run() gets a first poll. The initial
    // statistics sample then detects that the webview channel has closed.
    control.request_lease(LeaseKind::Input).await.unwrap();
    let actor = Arc::new(AttachmentActor::new(
      "closed-channel".into(),
      "main".into(),
      ConnectionTargetDto::Local,
      control,
    ));
    let channel =
      Channel::new(|_| Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into()));
    let server = async {
      loop {
        let request = read_frame::<_, ClientMessage>(&mut peer)
          .await
          .unwrap()
          .unwrap();
        if request == ClientMessage::Detach {
          write_frame(&mut peer, &ServerMessage::Detached)
            .await
            .unwrap();
          break;
        }
        assert_eq!(
          request,
          ClientMessage::AcquireLease {
            lease: LeaseKind::Input
          }
        );
      }
    };
    let forwarding = Box::pin(forward_attachment_events(
      AppState::default(),
      Arc::clone(&actor),
      events,
      channel,
      controller.run(),
    ));
    timeout(std::time::Duration::from_secs(1), async {
      tokio::join!(forwarding, server);
    })
    .await
    .expect("closed webview cleanup stopped polling a full command queue");
    assert!(actor.closed.load(Ordering::Acquire));
  }

  #[tokio::test(start_paused = true)]
  async fn final_draining_keeps_the_original_presentation_deadline() {
    let (client, _peer, attached) = buffered_test_attachment().await;
    let (_controller, control, _events) = ctmux_client::AttachmentController::new(
      client,
      &attached,
      ctmux_client::AttachmentControllerOptions::default(),
    )
    .unwrap();
    let actor = AttachmentActor::new(
      "final-frame".into(),
      "main".into(),
      ConnectionTargetDto::Local,
      control,
    );
    actor
      .set_pending(PresentationAcknowledgement::Output { sequence_end: 4 })
      .await
      .unwrap();
    tokio::time::advance(std::time::Duration::from_secs(29)).await;
    let channel = Channel::new(|_| Ok(()));
    let mut interval = statistics::sampling_interval();
    let error = timeout(
      std::time::Duration::from_secs(2),
      statistics::wait_for_presentation(&actor, &channel, &mut interval, true),
    )
    .await
    .expect("final draining restarted the presentation deadline")
    .unwrap_err();
    assert_eq!(error.code, "presentation_acknowledgement_timeout");
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

  #[tokio::test(start_paused = true)]
  async fn statistics_stay_live_without_postponing_a_stalled_renderer_timeout() {
    use ctmux_client::{AttachmentController, AttachmentControllerOptions};
    use ctmux_proto::{ClientMessage, ServerMessage, read_frame, write_frame};

    let (client, mut peer, attached) = buffered_test_attachment().await;
    let (controller, control, events) =
      AttachmentController::new(client, &attached, AttachmentControllerOptions::default()).unwrap();
    let actor = Arc::new(AttachmentActor::new(
      "slow-renderer".into(),
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
    let forwarding = tokio::spawn(forward_attachment_events(
      AppState::default(),
      Arc::clone(&actor),
      events,
      channel,
      controller.run(),
    ));
    write_frame(
      &mut peer,
      &ServerMessage::Output {
        sequence_start: 0,
        sequence_end: 4,
        data: b"slow".to_vec(),
      },
    )
    .await
    .unwrap();
    while receiver.recv().await.unwrap()["event_type"] != "output" {}
    // Deliberately leave the presentation unacknowledged. Statistics continue
    // over the same channel while ordinary event forwarding waits for it.
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let sample = loop {
      let event = receiver.recv().await.unwrap();
      if event["event_type"] == "attachment_statistics" {
        break event;
      }
    };
    assert_eq!(sample["attachment_id"], "slow-renderer");
    assert_eq!(sample["pending_events"], 1);
    assert!(
      sample["downloaded_bytes"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        > 4
    );

    tokio::time::advance(PRESENTATION_ACKNOWLEDGEMENT_TIMEOUT).await;
    let detach = timeout(
      std::time::Duration::from_secs(1),
      read_frame::<_, ClientMessage>(&mut peer),
    )
    .await
    .expect("statistics restarted the acknowledgement timeout")
    .unwrap();
    assert_eq!(detach, Some(ClientMessage::Detach));
    write_frame(&mut peer, &ServerMessage::Detached)
      .await
      .unwrap();
    timeout(std::time::Duration::from_secs(1), forwarding)
      .await
      .expect("the detached attachment did not close")
      .unwrap();
    let delivered: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
    assert!(delivered.iter().any(|event| {
      event["event_type"] == "attachment_error"
        && event["code"] == "presentation_acknowledgement_timeout"
    }));
    assert!(actor.closed.load(Ordering::Acquire));
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
    let first = state.reserve_window("main", "first").unwrap();
    state
      .open_attachment("main", "second", || Ok(()), async { Ok(()) })
      .await
      .unwrap();
    assert!(!*first.borrow());
    assert_eq!(state.registry.lock().unwrap().by_window["main"].len(), 2);
    state.release("main", "second");
    assert!(!*first.borrow());
    assert_eq!(state.registry.lock().unwrap().by_window["main"].len(), 1);
    state.cancel_opening("main", "first");
    assert!(*first.borrow());
  }

  #[tokio::test]
  async fn recovery_is_scoped_to_the_verified_window_route_and_terminal() {
    use ctmux_client::{AttachmentController, AttachmentControllerOptions};

    let (client, _peer, attached) = buffered_test_attachment().await;
    let (_, control, _) =
      AttachmentController::new(client, &attached, AttachmentControllerOptions::default()).unwrap();
    let target = ConnectionTargetDto::ssh("saved-alias");
    let mut pinned = target.clone();
    if let ConnectionTargetDto::Ssh { remote_info, .. } = &mut pinned {
      *remote_info = Some(Box::new(ctl_proto::RemoteIdentity {
        remote_id: "11111111-1111-4111-8111-111111111111".into(),
        agent_version: "1.0.0".into(),
        build: None,
        ctmux_restart_supported: false,
        bundle: None,
        protocols: Vec::new(),
      }));
    }
    let actor = Arc::new(
      AttachmentActor::new("old".into(), "main".into(), pinned.clone(), control)
        .with_resume(&attached),
    );
    let state = AppState::default();
    state.reserve_window("main", "old").unwrap();
    state
      .activate("main", "old", Arc::clone(&actor), None)
      .unwrap();
    assert_eq!(
      state.resume_attachment("main", "old", &target, "terminal"),
      None
    );
    state.finish_attachment(&actor, true);
    assert_eq!(
      state.actor("main", "old").err().unwrap().code,
      "attachment_closing"
    );

    let mut different_identity = pinned.clone();
    if let ConnectionTargetDto::Ssh {
      remote_info: Some(remote),
      ..
    } = &mut different_identity
    {
      remote.remote_id = "22222222-2222-4222-8222-222222222222".into();
    }
    let different_target = ConnectionTargetDto::ssh("other-alias");
    for (window, terminal, target) in [
      ("other", "terminal", &target),
      ("main", "other", &target),
      ("main", "terminal", &different_target),
      ("main", "terminal", &different_identity),
    ] {
      assert_eq!(
        state.resume_attachment(window, "old", target, terminal),
        None
      );
    }
    assert_eq!(
      state.resume_attachment("main", "old", &target, "terminal"),
      Some((pinned, "token".into())),
    );

    state.detach_attachment("main", "old").await.unwrap();
    state.detach_attachment("main", "old").await.unwrap();
    assert!(state.registry.lock().unwrap().by_window.is_empty());
  }

  #[tokio::test]
  async fn cancelled_replacement_keeps_recovery_until_activation() {
    use ctmux_client::{AttachmentController, AttachmentControllerOptions};

    let (client, _peer, attached) = buffered_test_attachment().await;
    let (_, control, _) =
      AttachmentController::new(client, &attached, AttachmentControllerOptions::default()).unwrap();
    let target = ConnectionTargetDto::Local;
    let actor = Arc::new(
      AttachmentActor::new("old".into(), "main".into(), target.clone(), control.clone())
        .with_resume(&attached),
    );
    let replacement = Arc::new(AttachmentActor::new(
      "new".into(),
      "main".into(),
      target.clone(),
      control,
    ));
    let state = AppState::default();
    state.reserve_window("main", "old").unwrap();
    state
      .activate("main", "old", Arc::clone(&actor), None)
      .unwrap();
    state.finish_attachment(&actor, true);

    let _cancelled = state.reserve_window("main", "new").unwrap();
    state.cancel_opening("main", "new");
    let error = state
      .activate("main", "new", Arc::clone(&replacement), Some("old"))
      .unwrap_err();
    assert_eq!(error.code, "attachment_cancelled");
    state.release("main", "new");
    assert!(
      state
        .resume_attachment("main", "old", &target, "terminal")
        .is_some()
    );

    state.reserve_window("main", "new").unwrap();
    state
      .activate("main", "new", replacement, Some("old"))
      .unwrap();
    assert_eq!(
      state.resume_attachment("main", "old", &target, "terminal"),
      None
    );
  }

  #[tokio::test]
  async fn closing_a_window_cancels_all_its_panes_only() {
    let state = AppState::default();
    let first = state.reserve_window("main", "first").unwrap();
    let second = state.reserve_window("main", "second").unwrap();
    let other = state.reserve_window("other", "first").unwrap();
    state.retire_attachment_window("main").await;
    assert!(*first.borrow());
    assert!(*second.borrow());
    assert!(!*other.borrow());
    assert!(
      !state
        .registry
        .lock()
        .unwrap()
        .by_window
        .contains_key("main")
    );
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
    state.cancel_opening("main", "stalled");
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
    let cancelled = state.reserve_window("main", "current").unwrap();
    state.cancel_opening("secondary", "current");
    state.cancel_opening("main", "old");
    assert!(!*cancelled.borrow());
    state.cancel_opening("main", "current");
    assert!(*cancelled.borrow());
    state.release("main", "current");
    let replacement = state.reserve_window("main", "replacement").unwrap();
    state.cancel_opening("main", "current");
    assert!(!*replacement.borrow());
  }

  #[tokio::test]
  async fn window_close_and_reload_fence_queued_opens() {
    for reload in [false, true] {
      let state = AppState::default();
      let transition = state.window_transition("main");
      let transition_guard = transition.lock().await;
      let opening = state.open_attachment(
        "main",
        "queued",
        || panic!("stale open published its ID"),
        async { Ok(()) },
      );
      tokio::pin!(opening);
      tokio::select! {
        biased;
        _ = &mut opening => panic!("open skipped the transition lock"),
        () = std::future::ready(()) => {}
      }

      if reload {
        state.attachment_page_started("main");
      } else {
        state.retire_attachment_window("main").await;
      }
      drop(transition_guard);
      assert_eq!(opening.await.unwrap_err().code, "attachment_cancelled");
      assert_eq!(state.reserve_window("main", "fresh").is_ok(), reload);
    }
  }

  #[tokio::test]
  async fn reloading_detaches_the_old_transport_before_opening_a_replacement() {
    use ctmux_client::{AttachmentController, AttachmentControllerOptions};
    use ctmux_proto::{ClientMessage, ServerMessage, read_frame, write_frame};
    let (client, mut peer, attached) = buffered_test_attachment().await;
    let (controller, control, events) =
      AttachmentController::new(client, &attached, AttachmentControllerOptions::default()).unwrap();
    let actor = Arc::new(AttachmentActor::new(
      "old".into(),
      "main".into(),
      ConnectionTargetDto::Local,
      control,
    ));
    let state = AppState::default();
    state.reserve_window("main", "old").unwrap();
    state
      .activate("main", "old", Arc::clone(&actor), None)
      .unwrap();
    let (output_sent, mut output) = tokio::sync::mpsc::channel(1);
    let channel = Channel::new(move |body| {
      if body.deserialize::<serde_json::Value>().unwrap()["event_type"] == "output" {
        let _ = output_sent.try_send(());
      }
      Ok(())
    });
    let bridge = tokio::spawn(forward_attachment_events(
      state.clone(),
      actor,
      events,
      channel,
      controller.run(),
    ));
    timeout(std::time::Duration::from_secs(2), async {
      write_frame(
        &mut peer,
        &ServerMessage::Output {
          sequence_start: 0,
          sequence_end: 4,
          data: b"test".to_vec(),
        },
      )
      .await
      .unwrap();
      assert_eq!(output.recv().await, Some(()));
      // The old document never acknowledges the event.
      state.attachment_page_started("main");
      let replacement = state.open_attachment("main", "new", || Ok(()), async { Ok(()) });
      tokio::pin!(replacement);
      tokio::select! {
        biased;
        _ = &mut replacement => panic!("replacement opened before the old transport detached"),
        () = std::future::ready(()) => {}
      }
      loop {
        if read_frame::<_, ClientMessage>(&mut peer)
          .await
          .unwrap()
          .unwrap()
          == ClientMessage::Detach
        {
          break;
        }
      }
      write_frame(&mut peer, &ServerMessage::Detached)
        .await
        .unwrap();
      replacement.await.unwrap();
      bridge.await.unwrap();
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn window_transitions_are_stable_and_window_scoped() {
    let state = AppState::default();
    let first = state.window_transition("main");
    let same_window = state.window_transition("main");
    let other_window = state.window_transition("secondary");

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
      started_at: tokio::time::Instant::now(),
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
