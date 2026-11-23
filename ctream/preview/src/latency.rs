use super::requests::Requests;
use ctream_client::transport::Transport;
use eframe::egui;
use std::{
  sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, RecvTimeoutError, Sender},
  },
  thread,
  time::Duration,
};

const SAMPLE_PERIOD: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reading {
  Pending,
  Measured(Duration),
  Unavailable,
}

enum Event {
  Sample(Option<Duration>),
  Close,
}

pub(super) struct Latency {
  reading: Arc<Mutex<Reading>>,
  events: Sender<Event>,
}

impl Latency {
  pub(super) fn start<T: Transport + Send + Sync + 'static>(
    transport: Arc<T>,
    ctx: &egui::Context,
  ) -> Self {
    Self::start_with_timeout(transport, ctx, PROBE_TIMEOUT)
  }

  fn start_with_timeout<T: Transport + Send + Sync + 'static>(
    transport: Arc<T>,
    ctx: &egui::Context,
    timeout: Duration,
  ) -> Self {
    let requests = Requests::new();
    let client = requests.client(transport);
    let (events, incoming) = mpsc::channel();
    let (sample_again, next) = mpsc::channel();
    let results = events.clone();
    thread::spawn(move || {
      let Ok(mut probe) = client.latency_probe() else {
        let _ = results.send(Event::Sample(None));
        return;
      };
      loop {
        let elapsed = probe.sample().ok();
        if results.send(Event::Sample(elapsed)).is_err()
          || elapsed.is_none()
          || next.recv().is_err()
        {
          return;
        }
      }
    });

    let reading = Arc::new(Mutex::new(Reading::Pending));
    let shared = Arc::clone(&reading);
    let ctx = ctx.clone();
    thread::spawn(move || {
      // Keep a separate owner of the scope so a blocking read can be canceled
      // even when the viewer is minimized and no paint passes are running.
      monitor(incoming, sample_again, &shared, &ctx, timeout);
      drop(requests);
    });
    Self { reading, events }
  }

  pub(super) fn reading(&self) -> Reading {
    *self.reading.lock().unwrap()
  }
}

impl Drop for Latency {
  fn drop(&mut self) {
    let _ = self.events.send(Event::Close);
  }
}

fn monitor(
  incoming: Receiver<Event>,
  sample_again: Sender<()>,
  reading: &Mutex<Reading>,
  ctx: &egui::Context,
  timeout: Duration,
) {
  loop {
    let sample = match incoming.recv_timeout(timeout) {
      Ok(Event::Sample(Some(elapsed))) => Reading::Measured(elapsed),
      Ok(Event::Sample(None)) | Err(RecvTimeoutError::Timeout) => Reading::Unavailable,
      Ok(Event::Close) | Err(RecvTimeoutError::Disconnected) => return,
    };
    *reading.lock().unwrap() = sample;
    ctx.request_repaint_of(super::view::viewer_id());
    if sample == Reading::Unavailable
      || !matches!(
        incoming.recv_timeout(SAMPLE_PERIOD),
        Err(RecvTimeoutError::Timeout)
      )
      || sample_again.send(()).is_err()
    {
      return;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctream_client::transport::{Cancellation, Channel};
  use std::io::{self, Read};

  struct BlockingProbe {
    started: Sender<()>,
    canceled: Sender<()>,
  }

  struct Reader(Receiver<()>);

  impl Read for Reader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
      let _ = self.0.recv();
      Ok(0)
    }
  }

  impl Transport for BlockingProbe {
    type Reader = Reader;
    type Writer = io::Sink;

    fn open(&self, _: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
      let (wake, waiting) = mpsc::channel();
      let canceled = self.canceled.clone();
      self.started.send(()).unwrap();
      Ok(Channel {
        reader: Reader(waiting),
        writer: io::sink(),
        cancellation: Cancellation::new(move || {
          let _ = wake.send(());
          let _ = canceled.send(());
        }),
      })
    }
  }

  fn blocking_probe() -> (Arc<BlockingProbe>, Receiver<()>, Receiver<()>) {
    let (started, start) = mpsc::channel();
    let (canceled, cancel) = mpsc::channel();
    (Arc::new(BlockingProbe { started, canceled }), start, cancel)
  }

  #[test]
  fn timeout_cancels_stalled_probe_without_a_paint_pass() {
    let (probe, started, canceled) = blocking_probe();
    let latency =
      Latency::start_with_timeout(probe, &egui::Context::default(), Duration::from_millis(250));
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    canceled.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(latency.reading(), Reading::Unavailable);
  }

  #[test]
  fn dropping_latency_cancels_pending_read() {
    let (probe, started, canceled) = blocking_probe();
    let latency = Latency::start(probe, &egui::Context::default());
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(latency);
    canceled.recv_timeout(Duration::from_secs(2)).unwrap();
  }
}
