//! Ordered disk persistence that does not hold up terminal presentation.
use crate::error::CommandResult;
use ctmux_client::{AttachmentControl, AttachmentEvent, cache::CacheIdentity};
use std::sync::{
  Arc,
  atomic::{AtomicBool, Ordering},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

const MAX_QUEUED_BYTES: u32 = 32 * 1024 * 1024;

struct PendingWrite {
  event: AttachmentEvent,
  _permit: OwnedSemaphorePermit,
}

pub(super) struct CacheWriter {
  sender: mpsc::Sender<PendingWrite>,
  capacity: Arc<Semaphore>,
  recovery_needed: Arc<AtomicBool>,
  control: AttachmentControl,
  task: tokio::task::JoinHandle<CommandResult<()>>,
}

impl CacheWriter {
  pub(super) fn new(identity: CacheIdentity, control: AttachmentControl) -> Self {
    let (sender, mut receiver) = mpsc::channel::<PendingWrite>(128);
    let capacity = Arc::new(Semaphore::new(MAX_QUEUED_BYTES as usize));
    let recovery_needed = Arc::new(AtomicBool::new(false));
    let worker_recovery = Arc::clone(&recovery_needed);
    let worker_control = control.clone();
    let task = tokio::spawn(async move {
      let mut desynchronized = false;
      let mut last_error = None;
      while let Some(write) = receiver.recv().await {
        let checkpoint = matches!(&write.event, AttachmentEvent::Checkpoint { .. });
        if desynchronized && !checkpoint {
          continue;
        }
        match crate::commands::cache::persist_event(identity.clone(), write.event).await {
          Ok(_) => desynchronized = false,
          Err(error) => {
            last_error = Some(error);
            desynchronized = true;
            request_recovery(&worker_recovery, &worker_control);
          }
        }
      }
      last_error.map_or(Ok(()), Err)
    });
    Self {
      sender,
      capacity,
      recovery_needed,
      control,
      task,
    }
  }

  pub(super) fn enqueue(&self, event: &AttachmentEvent) {
    if !matches!(
      event,
      AttachmentEvent::Checkpoint { .. }
        | AttachmentEvent::Output { .. }
        | AttachmentEvent::PtyGeometryChanged { .. }
        | AttachmentEvent::HistorySynced { .. }
    ) {
      return;
    }
    let checkpoint = matches!(event, AttachmentEvent::Checkpoint { .. });
    if self.recovery_needed.load(Ordering::Acquire) && !checkpoint {
      return;
    }
    let bytes = u32::try_from(event_bytes(event)).unwrap_or(u32::MAX).max(1);
    let Ok(permit) = Arc::clone(&self.capacity).try_acquire_many_owned(bytes) else {
      request_recovery(&self.recovery_needed, &self.control);
      return;
    };
    let write = PendingWrite {
      event: event.clone(),
      _permit: permit,
    };
    if self.sender.try_send(write).is_err() {
      request_recovery(&self.recovery_needed, &self.control);
    } else if checkpoint {
      self.recovery_needed.store(false, Ordering::Release);
    }
  }

  pub(super) async fn finish(self) -> CommandResult<()> {
    drop(self.sender);
    self
      .task
      .await
      .map_err(crate::error::CommandErrorDto::backend)?
  }
}

fn request_recovery(recovery_needed: &AtomicBool, control: &AttachmentControl) {
  if !recovery_needed.swap(true, Ordering::AcqRel) {
    let control = control.clone();
    tauri::async_runtime::spawn(async move {
      let _ignored = control.request_checkpoint().await;
    });
  }
}

fn event_bytes(event: &AttachmentEvent) -> usize {
  match event {
    AttachmentEvent::Output { data, .. } => data.len(),
    AttachmentEvent::Checkpoint {
      checkpoint,
      history,
      ..
    } => checkpoint
      .payload
      .len()
      .saturating_add(checkpoint.input_prefix.len())
      .saturating_add(history.lines.iter().map(String::len).sum::<usize>()),
    AttachmentEvent::HistorySynced {
      checkpoint,
      history,
      rows,
      ..
    } => checkpoint
      .payload
      .len()
      .saturating_add(checkpoint.input_prefix.len())
      .saturating_add(history.lines.iter().map(String::len).sum::<usize>())
      .saturating_add(rows.iter().map(|row| row.text.len()).sum::<usize>()),
    _ => 1,
  }
}
