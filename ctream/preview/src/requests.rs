use ctream_client::{
  Client,
  transport::{Cancellation, Channel, Transport},
};
use std::{
  io,
  sync::{Arc, Mutex},
};

#[derive(Default)]
struct State {
  ended: bool,
  channels: Vec<Cancellation>,
}

/// Owns all requests for one discovery or source selection. Clients may outlive
/// this guard, but cannot start another channel after it has been dropped.
#[derive(Default)]
pub(super) struct Requests {
  state: Arc<Mutex<State>>,
}

impl Requests {
  pub(super) fn new() -> Self {
    Self::default()
  }

  pub(super) fn client<T: Transport>(&self, transport: Arc<T>) -> Client<Scoped<T>> {
    Client::new(Scoped {
      transport,
      state: Arc::clone(&self.state),
    })
  }
}

impl Drop for Requests {
  fn drop(&mut self) {
    let channels = {
      let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
      state.ended = true;
      std::mem::take(&mut state.channels)
    };
    // Callbacks run outside the registry lock. Process cancellation requests
    // bounded cleanup without waiting for the child to exit on the UI thread.
    for channel in channels {
      channel.cancel();
    }
  }
}

/// Shares the underlying transport without extending its request scope.
/// Drop active previews before their Requests guard to close input gracefully.
pub(super) struct Scoped<T> {
  transport: Arc<T>,
  state: Arc<Mutex<State>>,
}

fn interrupted() -> io::Error {
  io::Error::new(io::ErrorKind::Interrupted, "preview request scope ended")
}

impl<T: Transport> Transport for Scoped<T> {
  type Reader = T::Reader;
  type Writer = T::Writer;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    if self
      .state
      .lock()
      .unwrap_or_else(|error| error.into_inner())
      .ended
    {
      return Err(interrupted());
    }

    // A custom transport may block inside open itself; cancellation can only
    // reach its channel when open returns. Process transports spawn immediately.
    let channel = self.transport.open(arguments)?;
    {
      let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
      if !state.ended {
        state.channels.push(channel.cancellation.clone());
        return Ok(channel);
      }
    }

    // The guard ended during open. Never hand this late channel to a reader or
    // let a legacy fallback start new work for the discarded selection.
    let Channel {
      reader,
      writer,
      cancellation,
    } = channel;
    drop(writer);
    cancellation.cancel();
    drop(reader);
    Err(interrupted())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{
    io::{Read, Write},
    sync::{
      Condvar,
      atomic::{AtomicUsize, Ordering},
      mpsc,
    },
    thread,
    time::Duration,
  };

  const TIMEOUT: Duration = Duration::from_secs(2);
  type Events = Arc<Mutex<Vec<&'static str>>>;

  struct ReadState {
    cancelled: Mutex<bool>,
    wake: Condvar,
    entered: mpsc::Sender<()>,
  }

  struct Reader {
    state: Arc<ReadState>,
    events: Events,
  }

  impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
      if bytes.is_empty() {
        return Ok(0);
      }
      let _ = self.state.entered.send(());
      let (cancelled, _) = self
        .state
        .wake
        .wait_timeout_while(self.state.cancelled.lock().unwrap(), TIMEOUT, |cancelled| {
          !*cancelled
        })
        .unwrap();
      if !*cancelled {
        return Err(io::Error::new(
          io::ErrorKind::TimedOut,
          "mock read was not cancelled",
        ));
      }
      Ok(0)
    }
  }

  impl Drop for Reader {
    fn drop(&mut self) {
      self.events.lock().unwrap().push("reader");
    }
  }

  struct Writer(Events);

  impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  impl Drop for Writer {
    fn drop(&mut self) {
      self.0.lock().unwrap().push("writer");
    }
  }

  struct OpenGate {
    entered: mpsc::Sender<()>,
    proceed: Mutex<mpsc::Receiver<()>>,
  }

  struct Mock {
    opens: AtomicUsize,
    events: Events,
    read: Arc<ReadState>,
    gate: Option<OpenGate>,
  }

  impl Mock {
    fn new(gate: Option<OpenGate>) -> (Arc<Self>, mpsc::Receiver<()>) {
      let (entered, waiting) = mpsc::channel();
      (
        Arc::new(Self {
          opens: AtomicUsize::new(0),
          events: Events::default(),
          read: Arc::new(ReadState {
            cancelled: Mutex::new(false),
            wake: Condvar::new(),
            entered,
          }),
          gate,
        }),
        waiting,
      )
    }
  }

  impl Transport for Mock {
    type Reader = Reader;
    type Writer = Writer;

    fn open(&self, _: &[String]) -> io::Result<Channel<Reader, Writer>> {
      self.opens.fetch_add(1, Ordering::SeqCst);
      if let Some(gate) = &self.gate {
        gate.entered.send(()).unwrap();
        gate
          .proceed
          .lock()
          .unwrap()
          .recv_timeout(TIMEOUT)
          .map_err(io::Error::other)?;
      }
      let read = Arc::clone(&self.read);
      let events = Arc::clone(&self.events);
      Ok(Channel {
        reader: Reader {
          state: Arc::clone(&read),
          events: Arc::clone(&events),
        },
        writer: Writer(Arc::clone(&events)),
        cancellation: Cancellation::new(move || {
          events.lock().unwrap().push("cancel");
          *read.cancelled.lock().unwrap() = true;
          read.wake.notify_all();
        }),
      })
    }
  }

  #[test]
  fn dropping_scope_unblocks_discovery_and_prevents_legacy_retry() {
    let requests = Requests::new();
    let (transport, reading) = Mock::new(None);
    let client = requests.client(Arc::clone(&transport));
    let (finished, result) = mpsc::channel();
    let worker = thread::spawn(move || {
      finished.send(client.discover().is_err()).unwrap();
    });
    reading
      .recv_timeout(TIMEOUT)
      .expect("discovery must reach its blocked read");
    drop(requests);
    assert!(
      result
        .recv_timeout(TIMEOUT)
        .expect("scope cancellation must unblock discovery")
    );
    worker.join().unwrap();
    assert_eq!(transport.opens.load(Ordering::SeqCst), 1);
    assert_eq!(
      &*transport.events.lock().unwrap(),
      &["writer", "cancel", "reader"]
    );
  }

  #[test]
  fn channel_returned_after_scope_ends_is_closed_without_being_read() {
    let requests = Requests::new();
    let (entered, opening) = mpsc::channel();
    let (proceed, gate) = mpsc::channel();
    let (transport, reading) = Mock::new(Some(OpenGate {
      entered,
      proceed: Mutex::new(gate),
    }));
    let client = requests.client(Arc::clone(&transport));
    let (finished, result) = mpsc::channel();
    let worker = thread::spawn(move || {
      finished
        .send(client.discover().unwrap_err().kind())
        .unwrap();
    });
    opening
      .recv_timeout(TIMEOUT)
      .expect("transport open must begin");
    drop(requests);
    proceed.send(()).unwrap();
    assert_eq!(
      result.recv_timeout(TIMEOUT).unwrap(),
      io::ErrorKind::Interrupted
    );
    worker.join().unwrap();
    assert_eq!(transport.opens.load(Ordering::SeqCst), 1);
    assert!(reading.try_recv().is_err());
    assert_eq!(
      &*transport.events.lock().unwrap(),
      &["writer", "cancel", "reader"]
    );
  }

  #[test]
  fn surviving_client_clones_cannot_open_channels_after_scope_ends() {
    let requests = Requests::new();
    let (transport, _) = Mock::new(None);
    let client = requests.client(Arc::clone(&transport));
    let clone = client.clone();
    drop(requests);
    assert_eq!(
      client.discover().unwrap_err().kind(),
      io::ErrorKind::Interrupted
    );
    let session = clone.session(ctream_proto::Target::X11(1), None);
    assert_eq!(
      session
        .capture()
        .err()
        .expect("capture must be cancelled")
        .kind(),
      io::ErrorKind::Interrupted
    );
    assert_eq!(transport.opens.load(Ordering::SeqCst), 0);
    assert!(transport.events.lock().unwrap().is_empty());
  }
}
