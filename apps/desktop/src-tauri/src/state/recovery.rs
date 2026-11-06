use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ctmux_client::AttachedSession;
use tokio::time::{Instant, sleep_until};

use super::{AppState, AttachmentActor, AttachmentSlot};
use crate::dto::ConnectionTargetDto;
use crate::error::CommandResult;

const MAX_RESUME_RETENTION: Duration = Duration::from_mins(10);

pub(super) struct AttachmentResume {
  target: ConnectionTargetDto,
  terminal_id: String,
  token: String,
  retention: Duration,
}

impl AttachmentResume {
  fn matches(&self, target: &ConnectionTargetDto, terminal_id: &str) -> bool {
    if self.terminal_id != terminal_id {
      return false;
    }
    match (&self.target, target) {
      (ConnectionTargetDto::Local, ConnectionTargetDto::Local) => true,
      (
        ConnectionTargetDto::Ssh {
          remote_info: Some(verified),
          ..
        },
        ConnectionTargetDto::Ssh { .. },
      ) => {
        if target.verify_remote_id(&verified.remote_id).is_err() {
          return false;
        }
        // Connection settings must match. The same verified remote may be
        // represented by an unpinned request or by newer build metadata.
        let mut target = target.clone();
        if let ConnectionTargetDto::Ssh { remote_info, .. } = &mut target {
          *remote_info = Some(verified.clone());
        }
        target == self.target
      }
      _ => false,
    }
  }
}

impl AttachmentActor {
  pub fn with_resume(mut self, attached: &AttachedSession) -> Self {
    self.resume = Some(Arc::new(AttachmentResume {
      target: self.target.clone(),
      terminal_id: attached.session.terminal_id.clone(),
      token: attached.attachment_token.clone(),
      // Allow for the daemon to detect a disconnect later than the client
      // and then reserve the lease for another liveness interval.
      retention: attached
        .liveness
        .peer_timeout
        .saturating_mul(2)
        .min(MAX_RESUME_RETENTION),
    }));
    self
  }
}

impl AppState {
  pub fn resume_attachment(
    &self,
    window_label: &str,
    attachment_id: &str,
    target: &ConnectionTargetDto,
    terminal_id: &str,
  ) -> Option<(ConnectionTargetDto, String)> {
    let registry = self.registry.lock().unwrap();
    let Some(AttachmentSlot::Resumable(resume)) = registry
      .by_window
      .get(window_label)
      .and_then(|slots| slots.get(attachment_id))
    else {
      return None;
    };
    resume
      .matches(target, terminal_id)
      .then(|| (resume.target.clone(), resume.token.clone()))
  }

  pub fn forget_resume(&self, window_label: &str, attachment_id: &str) {
    let mut registry = self.registry.lock().unwrap();
    if let Some(slots) = registry.by_window.get_mut(window_label) {
      remove_resume(slots, attachment_id);
      if slots.is_empty() {
        registry.by_window.remove(window_label);
      }
    }
  }

  pub(super) fn finish_attachment(&self, actor: &Arc<AttachmentActor>, disconnected: bool) {
    let mut registry = self.registry.lock().unwrap();
    let Some(slots) = registry.by_window.get_mut(&actor.window_label) else {
      return;
    };
    if !matches!(slots.get(&actor.attachment_id), Some(AttachmentSlot::Active(active)) if Arc::ptr_eq(active, actor))
    {
      return;
    }

    if disconnected
      && !actor.detaching.load(Ordering::Acquire)
      && let Some(resume) = &actor.resume
    {
      slots.insert(
        actor.attachment_id.clone(),
        AttachmentSlot::Resumable(Arc::clone(resume)),
      );
      let state = self.clone();
      let window_label = actor.window_label.clone();
      let attachment_id = actor.attachment_id.clone();
      let expires_at = Instant::now() + resume.retention;
      tokio::spawn(async move {
        sleep_until(expires_at).await;
        state.forget_resume(&window_label, &attachment_id);
      });
    } else {
      slots.remove(&actor.attachment_id);
      if slots.is_empty() {
        registry.by_window.remove(&actor.window_label);
      }
    }
  }

  pub async fn detach_attachment(
    &self,
    window_label: &str,
    attachment_id: &str,
  ) -> CommandResult<()> {
    let actor = {
      let mut registry = self.registry.lock().unwrap();
      let Some(slots) = registry.by_window.get_mut(window_label) else {
        return Ok(());
      };
      if remove_resume(slots, attachment_id) {
        if slots.is_empty() {
          registry.by_window.remove(window_label);
        }
        return Ok(());
      }
      let Some(AttachmentSlot::Active(actor)) = slots.get(attachment_id) else {
        return Ok(());
      };
      // Mark under the registry lock so a simultaneous disconnect cannot
      // preserve a credential for an attachment the renderer has closed.
      actor.detaching.store(true, Ordering::Release);
      Arc::clone(actor)
    };
    actor.detach_and_wait().await
  }
}

pub(super) fn remove_resume(
  slots: &mut HashMap<String, AttachmentSlot>,
  attachment_id: &str,
) -> bool {
  if matches!(slots.get(attachment_id), Some(AttachmentSlot::Resumable(_))) {
    slots.remove(attachment_id);
    true
  } else {
    false
  }
}
