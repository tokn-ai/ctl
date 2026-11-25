//! Keep each window input channel bound to the capture that authorized it.

use ctream_driver::Result;
use ctream_driver_input::macos::WindowIdentity;
use std::{
  collections::{BTreeMap, btree_map::Entry},
  net::Shutdown,
  os::unix::net::UnixStream,
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
  },
};

pub(super) struct CaptureSession {
  identity: WindowIdentity,
  has_frame: AtomicBool,
  ended: AtomicBool,
  input: Mutex<Option<UnixStream>>,
}

impl CaptureSession {
  pub(super) fn new(identity: WindowIdentity) -> Self {
    Self {
      identity,
      has_frame: AtomicBool::new(false),
      ended: AtomicBool::new(false),
      input: Mutex::new(None),
    }
  }

  pub(super) fn received_frame(&self) {
    self.has_frame.store(true, Ordering::Release);
  }

  pub(super) fn end(&self) {
    if !self.ended.swap(true, Ordering::AcqRel)
      && let Some(stream) = self.input.lock().unwrap().as_ref()
    {
      let _ = stream.shutdown(Shutdown::Read);
    }
  }

  fn attach(self: Arc<Self>, identity: WindowIdentity, stream: &UnixStream) -> Result<InputLease> {
    if self.identity != identity {
      return Err("macOS window input must match the active window capture".into());
    }
    let mut input = self.input.lock().unwrap();
    if self.ended.load(Ordering::Acquire) {
      return Err("the active macOS window capture has ended".into());
    }
    if input.is_some() {
      return Err("a macOS window input channel is already open".into());
    }
    *input = Some(stream.try_clone()?);
    drop(input);
    Ok(InputLease(self))
  }
}

pub(super) struct CaptureRegistry(Mutex<BTreeMap<Option<String>, Arc<CaptureSession>>>);

impl CaptureRegistry {
  pub(super) const fn new() -> Self {
    Self(Mutex::new(BTreeMap::new()))
  }

  pub(super) fn start(
    &self,
    lease: Option<&str>,
    session: Arc<CaptureSession>,
  ) -> Result<CaptureLease<'_>> {
    // Retained captures use the single None slot. Named captures are indexed
    // by the opaque session lease, independently of the selected window ID.
    let key = lease.map(str::to_owned);
    match self.0.lock().unwrap().entry(key.clone()) {
      Entry::Occupied(_) => Err("a macOS window capture is already open for this session".into()),
      Entry::Vacant(entry) => {
        entry.insert(Arc::clone(&session));
        Ok(CaptureLease {
          registry: self,
          key,
          session,
        })
      }
    }
  }

  pub(super) fn attach(
    &self,
    lease: Option<&str>,
    identity: WindowIdentity,
    stream: &UnixStream,
  ) -> Result<InputLease> {
    let session = self
      .0
      .lock()
      .unwrap()
      .get(&lease.map(str::to_owned))
      .cloned()
      .ok_or("macOS window input requires an active capture for this session")?;
    if lease.is_some() && !session.has_frame.load(Ordering::Acquire) {
      return Err("macOS session input requires a frame from the selected window".into());
    }
    session.attach(identity, stream)
  }
}

pub(super) struct CaptureLease<'a> {
  registry: &'a CaptureRegistry,
  key: Option<String>,
  session: Arc<CaptureSession>,
}

impl Drop for CaptureLease<'_> {
  fn drop(&mut self) {
    self.session.end();
    self.registry.0.lock().unwrap().remove(&self.key);
  }
}

pub(super) struct InputLease(Arc<CaptureSession>);

impl InputLease {
  pub(super) fn ended(&self) -> bool {
    self.0.ended.load(Ordering::Acquire)
  }
}

impl Drop for InputLease {
  fn drop(&mut self) {
    self.0.input.lock().unwrap().take();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{io::Read, time::Duration};

  #[test]
  fn capture_end_releases_input_for_the_selected_window() {
    let identity = WindowIdentity { id: 7, pid: 10 };
    let session = Arc::new(CaptureSession::new(identity));
    let (peer, mut input) = UnixStream::pair().unwrap();
    assert!(
      Arc::clone(&session)
        .attach(WindowIdentity { id: 7, pid: 11 }, &input)
        .is_err()
    );
    let lease = Arc::clone(&session).attach(identity, &input).unwrap();
    assert!(Arc::clone(&session).attach(identity, &input).is_err());
    session.end();
    // Keep the client open and prove that stopping capture ends the receiver.
    input
      .set_read_timeout(Some(Duration::from_secs(1)))
      .unwrap();
    assert_eq!(input.read(&mut [0]).unwrap(), 0);
    drop(peer);
    drop(lease);
    assert!(session.attach(identity, &input).is_err());
  }

  #[test]
  fn named_captures_and_input_remain_independent_for_shared_and_distinct_windows() {
    const FIRST: Option<&str> = Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const SECOND: Option<&str> = Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let first_id = WindowIdentity { id: 7, pid: 10 };
    let different = WindowIdentity { id: 8, pid: 11 };
    for second_id in [first_id, different] {
      let registry = CaptureRegistry::new();
      let first = registry
        .start(FIRST, Arc::new(CaptureSession::new(first_id)))
        .unwrap();
      let second = registry
        .start(SECOND, Arc::new(CaptureSession::new(second_id)))
        .unwrap();
      assert!(
        registry
          .start(FIRST, Arc::new(CaptureSession::new(second_id)))
          .is_err()
      );
      let (_first_peer, mut first_socket) = UnixStream::pair().unwrap();
      let (mut second_peer, mut second_socket) = UnixStream::pair().unwrap();
      assert!(registry.attach(FIRST, first_id, &first_socket).is_err());
      first.session.received_frame();
      second.session.received_frame();
      assert!(registry.attach(None, first_id, &first_socket).is_err());
      assert!(registry.attach(FIRST, different, &first_socket).is_err());
      let first_input = registry.attach(FIRST, first_id, &first_socket).unwrap();
      let second_input = registry.attach(SECOND, second_id, &second_socket).unwrap();
      assert!(registry.attach(FIRST, first_id, &first_socket).is_err());

      drop(first);
      assert!(first_input.ended());
      first_socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
      assert_eq!(first_socket.read(&mut [0]).unwrap(), 0);
      assert!(registry.attach(FIRST, first_id, &first_socket).is_err());
      assert!(!second_input.ended());
      std::io::Write::write_all(&mut second_peer, b"x").unwrap();
      let mut byte = [0];
      second_socket.read_exact(&mut byte).unwrap();
      assert_eq!(&byte, b"x");
      drop(second);
      assert!(second_input.ended());
    }
  }
}
