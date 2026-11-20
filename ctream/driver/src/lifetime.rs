use std::{
  cell::Cell,
  io::{self, Read},
  sync::mpsc::{self, Receiver, RecvTimeoutError},
  thread,
  time::Duration,
};

/// Cancellation observed by an owned capture resource.
pub struct CaptureLifetime {
  closed: Receiver<()>,
  cancelled: Cell<bool>,
}

impl CaptureLifetime {
  /// A message or disconnection cancels the capture. The host owns the sender.
  pub fn from_receiver(closed: Receiver<()>) -> Self {
    Self {
      closed,
      cancelled: Cell::new(false),
    }
  }

  /// Adapt EOF or an input error into cancellation, as used by stdio agents.
  pub fn from_reader(mut input: impl Read + Send + 'static) -> Self {
    let (notify, closed) = mpsc::channel();
    thread::spawn(move || {
      let _ = io::copy(&mut input, &mut io::sink());
      let _ = notify.send(());
    });
    Self::from_receiver(closed)
  }

  pub fn cancelled(&self, delay: Duration) -> bool {
    if !self.cancelled.get()
      && !matches!(
        self.closed.recv_timeout(delay),
        Err(RecvTimeoutError::Timeout)
      )
    {
      self.cancelled.set(true);
    }
    self.cancelled.get()
  }
}
