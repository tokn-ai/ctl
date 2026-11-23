use std::{
  io::{self, Read},
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError},
  },
  thread,
  time::Duration,
};

/// Cancellation observed by an owned capture resource.
pub struct CaptureLifetime {
  closed: Receiver<()>,
  cancellation: CaptureCancellation,
}

#[derive(Clone, Default)]
pub struct CaptureCancellation(Arc<AtomicBool>);

impl CaptureCancellation {
  pub fn cancel(&self) {
    self.0.store(true, Ordering::Release);
  }

  pub fn is_cancelled(&self) -> bool {
    self.0.load(Ordering::Acquire)
  }
}

impl CaptureLifetime {
  /// A message or disconnection cancels the capture. The host owns the sender.
  pub fn from_receiver(closed: Receiver<()>) -> Self {
    Self {
      closed,
      cancellation: CaptureCancellation::default(),
    }
  }

  /// Adapt EOF or an input error into cancellation, as used by stdio agents.
  pub fn from_reader(mut input: impl Read + Send + 'static) -> Self {
    let (notify, closed) = mpsc::channel();
    let lifetime = Self::from_receiver(closed);
    let cancellation = lifetime.cancellation();
    thread::spawn(move || {
      let _ = io::copy(&mut input, &mut io::sink());
      cancellation.cancel();
      let _ = notify.send(());
    });
    lifetime
  }

  pub fn cancelled(&self, delay: Duration) -> bool {
    if !self.cancellation.is_cancelled()
      && !matches!(
        self.closed.recv_timeout(delay),
        Err(RecvTimeoutError::Timeout)
      )
    {
      self.cancellation.cancel();
    }
    self.cancellation.is_cancelled()
  }

  pub fn cancellation(&self) -> CaptureCancellation {
    self.cancellation.clone()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reader_eof_cancels_workers_without_polling_the_source() {
    let lifetime = CaptureLifetime::from_reader(io::empty());
    let cancellation = lifetime.cancellation();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !cancellation.is_cancelled() {
      assert!(
        std::time::Instant::now() < deadline,
        "EOF did not cancel the worker"
      );
      thread::sleep(Duration::from_millis(1));
    }
  }
}
