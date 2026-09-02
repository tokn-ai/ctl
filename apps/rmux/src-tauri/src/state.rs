use rmux_client::{
  AttachmentAcknowledgementError, AttachmentControl, AttachmentEvent, AttachmentEvents,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager as _;
use tauri::ipc::Channel;
use tokio::sync::{Mutex, Notify};
use tokio::time::{sleep, timeout};

use crate::dto::{AttachmentEventDto, ConnectionTargetDto, PresentationAcknowledgement};
use crate::error::{CommandErrorDto, CommandResult};

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
  by_window: HashMap<String, AttachmentSlot>,
  window_transitions: HashMap<String, Arc<Mutex<()>>>,
}

enum AttachmentSlot {
  Opening { attachment_id: String },
  Active(Arc<AttachmentActor>),
}

pub struct AttachmentActor {
  pub attachment_id: String,
  pub window_label: String,
  pub target: ConnectionTargetDto,
  pub control: AttachmentControl,
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

  pub async fn detach_active_window(&self, window_label: &str) -> CommandResult<()> {
    let actor = {
      let registry = self.registry.lock().await;
      match registry.by_window.get(window_label) {
        Some(AttachmentSlot::Opening { .. }) => {
          return Err(CommandErrorDto::new(
            "window_attachment_transition_in_progress",
            "another attachment transition is already in progress for this window",
          ));
        }
        Some(AttachmentSlot::Active(actor)) => Some(Arc::clone(actor)),
        None => None,
      }
    };

    match actor {
      Some(actor) => actor.detach_and_wait().await,
      None => Ok(()),
    }
  }

  /// Detaches the active actor only when it belongs to the local daemon being
  /// restarted. A remote SSH attachment in the same window is unrelated and
  /// must remain live through local maintenance.
  pub async fn detach_active_local_window(&self, window_label: &str) -> CommandResult<()> {
    let actor = {
      let registry = self.registry.lock().await;
      match registry.by_window.get(window_label) {
        Some(AttachmentSlot::Opening { .. }) => {
          return Err(CommandErrorDto::new(
            "window_attachment_transition_in_progress",
            "another attachment transition is already in progress for this window",
          ));
        }
        Some(AttachmentSlot::Active(actor)) if actor.target.is_local() => Some(Arc::clone(actor)),
        Some(AttachmentSlot::Active(_)) | None => None,
      }
    };

    match actor {
      Some(actor) => actor.detach_and_wait().await,
      None => Ok(()),
    }
  }

  pub async fn reserve_window(&self, window_label: &str, attachment_id: &str) -> CommandResult<()> {
    let mut registry = self.registry.lock().await;
    if registry.by_window.contains_key(window_label) {
      return Err(CommandErrorDto::new(
        "window_already_attached",
        "another attachment transition is already in progress for this window",
      ));
    }
    registry.by_window.insert(
      window_label.into(),
      AttachmentSlot::Opening {
        attachment_id: attachment_id.into(),
      },
    );
    Ok(())
  }

  pub async fn activate(
    &self,
    window_label: &str,
    attachment_id: &str,
    actor: Arc<AttachmentActor>,
  ) -> CommandResult<()> {
    let mut registry = self.registry.lock().await;
    let reservation_matches = matches!(
      registry.by_window.get(window_label),
      Some(AttachmentSlot::Opening { attachment_id: reserved }) if reserved == attachment_id
    );
    if !reservation_matches {
      return Err(CommandErrorDto::new(
        "attachment_reservation_lost",
        "the attachment window reservation is no longer active",
      ));
    }
    registry
      .by_window
      .insert(window_label.into(), AttachmentSlot::Active(actor));
    Ok(())
  }

  pub async fn release(&self, window_label: &str, attachment_id: &str) {
    let mut registry = self.registry.lock().await;
    let should_remove = match registry.by_window.get(window_label) {
      Some(AttachmentSlot::Opening {
        attachment_id: current,
      }) => current == attachment_id,
      Some(AttachmentSlot::Active(actor)) => actor.attachment_id == attachment_id,
      None => false,
    };
    if should_remove {
      registry.by_window.remove(window_label);
    }
  }

  pub async fn actor(
    &self,
    window_label: &str,
    attachment_id: &str,
  ) -> CommandResult<Arc<AttachmentActor>> {
    let registry = self.registry.lock().await;
    let Some(AttachmentSlot::Active(actor)) = registry.by_window.get(window_label) else {
      return Err(CommandErrorDto::new(
        "attachment_not_found",
        "this window has no active attachment",
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
    let actor = {
      let mut registry = self.registry.lock().await;
      match registry.by_window.get(window_label) {
        Some(AttachmentSlot::Opening { .. }) => {
          registry.by_window.remove(window_label);
          None
        }
        Some(AttachmentSlot::Active(actor)) => Some(Arc::clone(actor)),
        None => None,
      }
    };
    if let Some(actor) = actor {
      let _ignored = actor.control.detach().await;
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
      pending: Mutex::new(None),
      pending_changed: Notify::new(),
      closed: AtomicBool::new(false),
      closed_changed: Notify::new(),
    }
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

    let result = acknowledgement.apply(&self.control).await;
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
    Output = Result<rmux_client::AttachExit, rmux_client::ClientError>,
  >,
) {
  tokio::pin!(controller);

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
        if let Err(error) = forward_event(&actor, &channel, event).await {
          bridge_error = Some(error);
          let _ignored = actor.control.detach().await;
          break controller.await;
        }
      }
      result = &mut controller => break result,
    }
  };

  let require_checkpoint = actor.has_pending_presentation().await;
  actor.clear_pending().await;
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
  state
    .release(&actor.window_label, &actor.attachment_id)
    .await;
  actor.mark_closed();
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
        history_gap,
      )
    }
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
    AttachmentEvent::LeaseStatus { lease, status } => {
      AttachmentEventDto::lease_status(&actor.attachment_id, lease, status)
    }
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
    };
    assert!(matches!(
      slot,
      AttachmentSlot::Opening { attachment_id } if attachment_id == "expected"
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
