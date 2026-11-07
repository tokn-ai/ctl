//! Give each renderer document its own attachment lifetime. A `WebView` may
//! navigate without destroying its window, including when it reloads.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::{Mutex, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;

use super::{
  ATTACHMENT_CLOSE_TIMEOUT, AppState, AttachmentActor, AttachmentRegistry, AttachmentSlot,
};
use crate::error::{CommandErrorDto, CommandResult};

pub(super) struct WindowDocument {
  accepting_attachments: bool,
  drain: Option<watch::Receiver<bool>>,
}

impl WindowDocument {
  pub(super) async fn wait_for_previous(&self) {
    if let Some(mut drained) = self.drain.clone() {
      let _ = drained.wait_for(|done| *done).await;
    }
  }
}

pub(super) fn stale_document() -> CommandErrorDto {
  CommandErrorDto::new(
    "attachment_cancelled",
    "The terminal window closed or reloaded while opening the session.",
  )
}

impl AttachmentRegistry {
  pub(super) fn is_current_document(&self, label: &str, document: &Arc<WindowDocument>) -> bool {
    !self.exiting
      && document.accepting_attachments
      && self
        .documents
        .get(label)
        .is_some_and(|current| Arc::ptr_eq(current, document))
  }

  fn replace_document(&mut self, label: &str, accepting_attachments: bool) -> Arc<WindowDocument> {
    let previous = self.documents.get(label);
    if let Some(current) = previous
      && !current.accepting_attachments
    {
      return Arc::clone(current);
    }

    let previous_drain = previous.and_then(|document| document.drain.clone());
    let mut actors = Vec::new();
    let mut cancelled_opening = false;
    if let Some(slots) = self.by_window.remove(label) {
      for slot in slots.into_values() {
        match slot {
          AttachmentSlot::Opening { cancel, .. } => {
            cancel.send_replace(true);
            cancelled_opening = true;
          }
          AttachmentSlot::Active(actor) => {
            actor.detaching.store(true, Ordering::Release);
            actor.renderer_retired.send_replace(true);
            actors.push(actor);
          }
          AttachmentSlot::Resumable(_) => {}
        }
      }
    }

    let opening = self
      .window_transitions
      .get(label)
      .filter(|_| cancelled_opening)
      .cloned();
    let drain = if actors.is_empty() && opening.is_none() {
      previous_drain
    } else {
      let (finished, waiting) = watch::channel(false);
      tauri::async_runtime::spawn(async move {
        detach_retired(actors, opening).await;
        finished.send_replace(true);
      });
      Some(waiting)
    };
    let document = Arc::new(WindowDocument {
      accepting_attachments,
      drain,
    });
    self.documents.insert(label.into(), Arc::clone(&document));
    document
  }
}

impl AppState {
  pub(super) fn current_document(&self, label: &str) -> CommandResult<Arc<WindowDocument>> {
    let mut registry = self.registry.lock().unwrap();
    if registry.exiting {
      return Err(stale_document());
    }
    let document = registry.documents.entry(label.into()).or_insert_with(|| {
      Arc::new(WindowDocument {
        accepting_attachments: true,
        drain: None,
      })
    });
    if !document.accepting_attachments {
      return Err(stale_document());
    }
    Ok(Arc::clone(document))
  }

  pub(crate) fn attachment_page_started(&self, label: &str) {
    let mut registry = self.registry.lock().unwrap();
    if !registry.exiting {
      registry.replace_document(label, true);
    }
  }

  pub(crate) fn retire_attachment_window(&self, label: &str) -> impl Future<Output = ()> + use<> {
    let document = self.registry.lock().unwrap().replace_document(label, false);
    async move { document.wait_for_previous().await }
  }

  pub(crate) fn reopen_after_close_failure(&self, label: &str) {
    let mut registry = self.registry.lock().unwrap();
    if !registry.exiting {
      registry.documents.remove(label);
    }
  }

  pub(crate) fn retire_all_attachment_windows(&self) -> impl Future<Output = ()> + use<> {
    let documents = {
      let mut registry = self.registry.lock().unwrap();
      registry.exiting = true;
      let labels: Vec<_> = registry.documents.keys().cloned().collect();
      labels
        .iter()
        .map(|label| registry.replace_document(label, false))
        .collect::<Vec<_>>()
    };
    async move {
      // Each document starts draining before we await any one of them.
      for document in documents {
        document.wait_for_previous().await;
      }
    }
  }
}

async fn detach_retired(actors: Vec<Arc<AttachmentActor>>, opening: Option<Arc<Mutex<()>>>) {
  let mut detachments = JoinSet::new();
  for actor in &actors {
    let actor = Arc::clone(actor);
    detachments.spawn(async move {
      if actor.detach_and_wait().await.is_err() {
        actor.abort_bridge.notify_one();
      }
    });
  }
  if let Some(opening) = opening {
    detachments.spawn(async move {
      // Cancellation is published before taking the lock. Wait for the
      // currently opening transport to drop before letting a new page attach.
      let _transition_guard = opening.lock().await;
    });
  }
  // Bound the whole drain, including time spent queueing Detach on a stalled
  // controller. Force-close remaining transports if the peer does not reply.
  if timeout(ATTACHMENT_CLOSE_TIMEOUT, async {
    while detachments.join_next().await.is_some() {}
  })
  .await
  .is_err()
  {
    for actor in &actors {
      actor.abort_bridge.notify_one();
    }
  }
}
