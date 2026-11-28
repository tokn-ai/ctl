//! A native source publishes its newest complete frame once. Each subscriber
//! observes its own cursor and can skip intermediate frames independently.
use ctream_driver::{CaptureLifetime, Frame};
use std::{
  collections::BTreeMap,
  io,
  sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
    mpsc,
  },
  thread,
};
use tokio::sync::watch;

type Update = Option<Result<Arc<Frame>, Arc<str>>>;

#[derive(Clone)]
pub struct Frames {
  updates: watch::Sender<Update>,
  status: SourceStatus,
}

impl Frames {
  pub(crate) fn channel() -> (Self, LatestFrames) {
    let (updates, latest) = watch::channel(None);
    (
      Self {
        updates,
        status: SourceStatus::default(),
      },
      LatestFrames(latest),
    )
  }

  /// Revoke input leases immediately when a native capture ends, including
  /// while a viewer is backpressured writing its previous frame.
  pub fn end(&self) {
    self.status.end();
  }

  /// Returns the old pixel allocation when every subscriber has finished with
  /// that frame. A native producer can reuse it without copying between viewers.
  pub fn publish(&self, frame: Frame) -> Vec<u8> {
    self
      .updates
      .send_replace(Some(Ok(Arc::new(frame))))
      .and_then(Result::ok)
      .and_then(Arc::into_inner)
      .map(|frame| frame.pixels)
      .unwrap_or_default()
  }
}

#[derive(Clone, Default)]
pub struct SourceStatus(Arc<AtomicBool>);

impl SourceStatus {
  fn end(&self) {
    self.0.store(true, Ordering::Release);
  }

  pub fn ended(&self) -> bool {
    self.0.load(Ordering::Acquire)
  }
}

struct Worker(SourceStatus);

impl Drop for Worker {
  fn drop(&mut self) {
    self.0.end();
  }
}

struct Entry {
  active: Weak<Active>,
  // A replacement capture waits for the old driver to finish releasing its
  // platform resources, independently of all the other capture sources.
  worker: Weak<Mutex<()>>,
}

struct Active {
  status: SourceStatus,
  latest: LatestFrames,
  _stop: mpsc::Sender<()>,
  cancellation: ctream_driver::CaptureCancellation,
}

impl Drop for Active {
  fn drop(&mut self) {
    self.cancellation.cancel();
  }
}

pub struct Pool<K>(Mutex<BTreeMap<K, Entry>>);

impl<K: Ord> Default for Pool<K> {
  fn default() -> Self {
    Self(Mutex::new(BTreeMap::new()))
  }
}

impl<K: Ord> Pool<K> {
  pub fn subscribe(
    &self,
    key: K,
    open: impl FnOnce(Frames, CaptureLifetime) -> io::Result<()> + Send + 'static,
  ) -> io::Result<Subscription> {
    let mut sources = self.0.lock().unwrap();
    sources
      .retain(|_, source| source.worker.strong_count() > 0 || source.active.strong_count() > 0);
    let entry = sources.get(&key);
    let existing = entry.and_then(|source| source.active.upgrade());
    let active = match existing
      .filter(|source| !source.status.ended() && source.latest.0.has_changed().is_ok())
    {
      Some(active) => active,
      None => {
        let (frames, latest) = Frames::channel();
        let updates = frames.updates.clone();
        let (stop, closed) = mpsc::channel();
        let lifetime = CaptureLifetime::from_receiver(closed);
        let worker = entry
          .and_then(|source| source.worker.upgrade())
          .unwrap_or_default();
        let worker_key = Arc::downgrade(&worker);
        let status = frames.status.clone();
        let active = Arc::new(Active {
          status: status.clone(),
          latest,
          _stop: stop,
          cancellation: lifetime.cancellation(),
        });
        thread::Builder::new()
          .name("ctream-capture-source".into())
          .spawn(move || {
            let _finished = Worker(status);
            let _exclusive = worker.lock().unwrap();
            if lifetime.cancelled(std::time::Duration::ZERO) {
              return;
            }
            if let Err(error) = open(frames, lifetime) {
              updates.send_replace(Some(Err(error.to_string().into())));
            }
          })?;
        sources.insert(
          key,
          Entry {
            active: Arc::downgrade(&active),
            worker: worker_key,
          },
        );
        active
      }
    };
    let mut latest = active.latest.clone();
    // A late subscriber can start immediately from the current complete frame.
    latest.0.mark_changed();
    Ok(Subscription {
      latest,
      _active: active,
    })
  }
}

pub struct Subscription {
  latest: LatestFrames,
  _active: Arc<Active>,
}

impl Subscription {
  pub fn status(&self) -> SourceStatus {
    self._active.status.clone()
  }

  pub async fn next(&mut self) -> io::Result<Option<Arc<Frame>>> {
    self.latest.next().await
  }
}

#[derive(Clone)]
pub(crate) struct LatestFrames(watch::Receiver<Update>);

impl LatestFrames {
  pub(crate) async fn next(&mut self) -> io::Result<Option<Arc<Frame>>> {
    while self.0.changed().await.is_ok() {
      if let Some(update) = self.0.borrow_and_update().clone() {
        return update
          .map(Some)
          .map_err(|message| io::Error::other(message.to_string()));
      }
    }
    Ok(None)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
  };

  #[tokio::test]
  async fn viewers_share_one_source_and_skip_to_the_latest_without_starving_each_other() {
    let pool = Pool::default();
    let (feed, incoming) = mpsc::channel();
    let (closed, finished) = mpsc::channel();
    let starts = Arc::new(AtomicUsize::new(0));
    let spawned = Arc::clone(&starts);
    let mut first = pool
      .subscribe(1, move |frames, life| {
        spawned.fetch_add(1, Ordering::Relaxed);
        while !life.cancelled(Duration::from_millis(1)) {
          if let Ok(value) = incoming.try_recv() {
            frames.publish(Frame {
              size: [1, 1],
              pixels: vec![value; 3],
            });
          }
        }
        closed.send(()).unwrap();
        Ok(())
      })
      .unwrap();
    feed.send(1).unwrap();
    let one = first.next().await.unwrap().unwrap();
    let mut slow = pool
      .subscribe(1, |_, _| panic!("duplicate source"))
      .unwrap();
    assert!(Arc::ptr_eq(&one, &slow.next().await.unwrap().unwrap()));
    let mut late = pool
      .subscribe(1, |_, _| panic!("duplicate source"))
      .unwrap();
    for value in 2..=5 {
      feed.send(value).unwrap();
      assert_eq!(first.next().await.unwrap().unwrap().pixels[0], value);
    }
    assert_eq!(slow.next().await.unwrap().unwrap().pixels[0], 5);
    assert_eq!(late.next().await.unwrap().unwrap().pixels[0], 5);
    drop((first, slow));
    assert!(finished.try_recv().is_err());
    drop(late);
    finished.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(starts.load(Ordering::Relaxed), 1);
    let mut reopened = pool
      .subscribe(1, |_, _| Err(io::Error::other("gone")))
      .unwrap();
    assert_eq!(reopened.next().await.err().unwrap().to_string(), "gone");
  }
}
