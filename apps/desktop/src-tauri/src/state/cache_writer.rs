//! Ordered disk persistence that does not hold up terminal presentation.
use crate::error::{CommandErrorDto, CommandResult};
use ctmux_client::{AttachmentControl, AttachmentEvent, cache::CacheIdentity};
use std::{
  future::Future,
  pin::Pin,
  sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

const MAX_QUEUED_BYTES: u32 = 32 * 1024 * 1024;
type WriteFuture = Pin<Box<dyn Future<Output = CommandResult<()>> + Send>>;
type Persist = Arc<dyn Fn(AttachmentEvent) -> WriteFuture + Send + Sync>;
type MarkGap = Arc<dyn Fn() -> WriteFuture + Send + Sync>;
type Request = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Recovery {
  epoch: u64,
  waiting: bool,
  requested: bool,
}

impl Recovery {
  fn invalidate(&mut self) {
    if !self.waiting {
      self.epoch += 1;
    }
    self.waiting = true;
  }

  fn request(&mut self, request: &Request) {
    if self.waiting && !self.requested {
      self.requested = true;
      request();
    }
  }
}

struct PendingWrite {
  event: AttachmentEvent,
  epoch: u64,
  permit: OwnedSemaphorePermit,
}

pub(super) struct CacheWriter {
  sender: mpsc::Sender<PendingWrite>,
  capacity: Arc<Semaphore>,
  recovery: Arc<Mutex<Recovery>>,
  request: Request,
  task: tokio::task::JoinHandle<CommandResult<()>>,
}

impl CacheWriter {
  pub(super) fn new(identity: CacheIdentity, control: AttachmentControl) -> Self {
    let persisted_identity = identity.clone();
    let persist: Persist = Arc::new(move |event| {
      let identity = persisted_identity.clone();
      Box::pin(async move {
        crate::commands::cache::persist_event(identity, event)
          .await
          .map(|_| ())
      })
    });
    let gap: MarkGap = Arc::new(move || {
      let identity = identity.clone();
      Box::pin(async move {
        tokio::task::spawn_blocking(move || {
          ctmux_client::cache::CacheStore::for_client("desktop")?.mark_history_gap(&identity)
        })
        .await
        .map_err(CommandErrorDto::backend)?
        .map_err(CommandErrorDto::backend)
      })
    });
    let request: Request = Arc::new(move || {
      let control = control.clone();
      tokio::spawn(async move {
        let _ignored = control.request_checkpoint().await;
      });
    });
    Self::start(persist, gap, request, 128, MAX_QUEUED_BYTES)
  }

  fn start(persist: Persist, gap: MarkGap, request: Request, slots: usize, bytes: u32) -> Self {
    let (sender, receiver) = mpsc::channel(slots);
    let capacity = Arc::new(Semaphore::new(bytes as usize));
    let recovery = Arc::new(Mutex::new(Recovery::default()));
    let task = tokio::spawn(run_writer(
      receiver,
      Arc::clone(&recovery),
      Arc::clone(&request),
      persist,
      gap,
    ));
    Self {
      sender,
      capacity,
      recovery,
      request,
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
    let mut recovery = self.recovery.lock().expect("cache recovery mutex poisoned");
    if recovery.waiting && !checkpoint {
      return;
    }
    let bytes = u32::try_from(event_bytes(event)).unwrap_or(u32::MAX).max(1);
    let sent = Arc::clone(&self.capacity)
      .try_acquire_many_owned(bytes)
      .is_ok_and(|permit| {
        self
          .sender
          .try_send(PendingWrite {
            event: event.clone(),
            epoch: recovery.epoch,
            permit,
          })
          .is_ok()
      });
    if sent {
      if checkpoint {
        recovery.waiting = false;
        recovery.requested = false;
      }
    } else {
      let already_waiting = recovery.waiting;
      recovery.invalidate();
      if checkpoint && already_waiting {
        // Its response could not fit. Retry after the worker releases space,
        // rather than requesting checkpoints in a loop while disk is slow.
        recovery.requested = false;
      } else {
        recovery.request(&self.request);
      }
    }
  }

  pub(super) async fn finish(self) -> CommandResult<()> {
    drop(self.sender);
    self.task.await.map_err(CommandErrorDto::backend)?
  }
}

async fn run_writer(
  mut receiver: mpsc::Receiver<PendingWrite>,
  recovery: Arc<Mutex<Recovery>>,
  request: Request,
  persist: Persist,
  gap: MarkGap,
) -> CommandResult<()> {
  let mut written_epoch = Some(0);
  let mut marked_epoch = None;
  let mut last_error = None;
  while let Some(write) = receiver.recv().await {
    let current_epoch = recovery
      .lock()
      .expect("cache recovery mutex poisoned")
      .epoch;
    if write.epoch == current_epoch {
      let checkpoint = matches!(write.event, AttachmentEvent::Checkpoint { .. });
      match persist(write.event).await {
        Ok(()) if checkpoint => {
          written_epoch = Some(write.epoch);
          last_error = None;
        }
        Ok(()) => {}
        Err(error) => {
          last_error = Some(error);
          recovery
            .lock()
            .expect("cache recovery mutex poisoned")
            .invalidate();
        }
      }
    }
    // Permits cover in-flight storage as well as queued attempts.
    drop(write.permit);
    let epoch = {
      let mut state = recovery.lock().expect("cache recovery mutex poisoned");
      state.request(&request);
      state.epoch
    };
    if written_epoch != Some(epoch) && marked_epoch != Some(epoch) {
      if let Err(error) = gap().await {
        last_error = Some(error);
      }
      marked_epoch = Some(epoch);
    }
  }
  let epoch = recovery
    .lock()
    .expect("cache recovery mutex poisoned")
    .epoch;
  if written_epoch != Some(epoch)
    && marked_epoch != Some(epoch)
    && let Err(error) = gap().await
  {
    last_error = Some(error);
  }
  last_error.map_or(Ok(()), Err)
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

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use tokio::sync::Notify;

  fn output(sequence: u64) -> AttachmentEvent {
    AttachmentEvent::Output {
      sequence_start: sequence,
      sequence_end: sequence + 1,
      data: vec![b'x'],
    }
  }

  fn checkpoint(sequence: u64) -> AttachmentEvent {
    AttachmentEvent::Checkpoint {
      checkpoint: ctmux_proto::TerminalCheckpoint {
        format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
        format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
        sequence,
        terminal_size: ctmux_proto::TerminalSize::default(),
        payload: vec![],
        input_prefix: vec![],
      },
      history: ctmux_proto::TerminalHistorySnapshot {
        format: ctmux_proto::TERMINAL_HISTORY_FORMAT.into(),
        format_version: ctmux_proto::TERMINAL_HISTORY_FORMAT_VERSION,
        sequence,
        generation: 0,
        revision: 0,
        retained_bytes: 0,
        lines: vec![],
        truncated: false,
      },
      history_manifest: None,
      history_gap: false,
    }
  }

  #[tokio::test]
  async fn slow_disk_invalidates_queued_output_and_retries_a_dropped_recovery_checkpoint() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let saved = Arc::new(Mutex::new(Vec::new()));
    let attempts = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let gaps = Arc::new(AtomicUsize::new(0));
    let persist: Persist = Arc::new({
      let (entered, release, saved, attempts) = (
        entered.clone(),
        release.clone(),
        saved.clone(),
        attempts.clone(),
      );
      move |event| {
        let (entered, release, saved, attempts) = (
          entered.clone(),
          release.clone(),
          saved.clone(),
          attempts.clone(),
        );
        Box::pin(async move {
          if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            entered.notify_one();
            release.notified().await;
          }
          saved.lock().unwrap().push(event);
          Ok(())
        })
      }
    });
    let gap: MarkGap = Arc::new({
      let gaps = gaps.clone();
      move || {
        gaps.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
      }
    });
    let request: Request = Arc::new({
      let requests = requests.clone();
      move || {
        requests.fetch_add(1, Ordering::SeqCst);
      }
    });
    let writer = CacheWriter::start(persist, gap, request, 1, 8);
    writer.enqueue(&checkpoint(0));
    entered.notified().await;
    writer.enqueue(&output(0));
    writer.enqueue(&output(1)); // Overflow: output(0) belongs to an obsolete epoch.
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    writer.enqueue(&checkpoint(2)); // The recovery response also cannot fit yet.
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
      while requests.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
      }
    })
    .await
    .unwrap();
    while writer.sender.capacity() == 0 {
      tokio::task::yield_now().await;
    }
    writer.enqueue(&checkpoint(2));
    writer.finish().await.unwrap();
    let saved = saved.lock().unwrap();
    assert_eq!(saved.len(), 2);
    assert!(
      matches!(saved[1], AttachmentEvent::Checkpoint { ref checkpoint, .. } if checkpoint.sequence == 2)
    );
    assert!(gaps.load(Ordering::SeqCst) > 0);
  }

  #[tokio::test]
  async fn unfinished_overflow_marks_the_archive_incomplete_at_shutdown() {
    let gaps = Arc::new(AtomicUsize::new(0));
    let gap: MarkGap = Arc::new({
      let gaps = gaps.clone();
      move || {
        gaps.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
      }
    });
    let writer = CacheWriter::start(
      Arc::new(|_| Box::pin(async { Ok(()) })),
      gap,
      Arc::new(|| {}),
      1,
      1,
    );
    writer.enqueue(&AttachmentEvent::Output {
      sequence_start: 0,
      sequence_end: 2,
      data: vec![b'x'; 2],
    });
    writer.finish().await.unwrap();
    assert_eq!(gaps.load(Ordering::SeqCst), 1);
  }
}
