//! Per-attempt key unlocking, requested only by an exact agent signature call.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex as SyncMutex, Weak};

use base64::Engine as _;
use tokio::sync::Mutex;

use super::fallback::{Fallbacks, Reason};
use crate::identities::{self, IdentitySnapshot, LocalAgent};

pub(super) struct Candidate {
  pub(super) snapshot: Arc<IdentitySnapshot>,
  pub(super) public_key: String,
  pub(super) blob: Vec<u8>,
}

impl Candidate {
  pub(super) fn new(snapshot: Arc<IdentitySnapshot>, public_key: String) -> Option<Self> {
    let blob = public_blob(&public_key)?;
    Some(Self {
      snapshot,
      public_key,
      blob,
    })
  }
}

/// The owner keeps the isolated agent alive only for this connection attempt.
/// This type deliberately contains neither the passphrase nor a Debug impl.
pub(super) struct UnlockedAgent {
  pub(super) public_key: String,
  pub(super) socket: PathBuf,
  pub(super) _owner: Box<dyn Send>,
}

type UnlockFuture = Pin<Box<dyn Future<Output = Result<UnlockedAgent, Reason>> + Send>>;

pub(super) trait Unlocker: Send + Sync {
  fn unlock(
    &self,
    snapshot: Arc<IdentitySnapshot>,
    context: String,
    canceled: Arc<AtomicBool>,
  ) -> UnlockFuture;
}

struct KeychainUnlocker;

struct CancelRead(Arc<AtomicBool>);

impl Drop for CancelRead {
  fn drop(&mut self) {
    self.0.store(true, Ordering::Release);
  }
}

impl Unlocker for KeychainUnlocker {
  fn unlock(
    &self,
    snapshot: Arc<IdentitySnapshot>,
    context: String,
    canceled: Arc<AtomicBool>,
  ) -> UnlockFuture {
    Box::pin(async move {
      // A blocking Keychain worker may outlive cancellation of this future.
      // Prevent a worker waiting for the operation lock from opening new UI.
      let selected = Arc::clone(&snapshot);
      let secret = tokio::task::spawn_blocking(move || {
        identities::saved_passphrase_cancellable(&selected, Some(&context), &canceled)
      })
      .await
      .map_err(|_| Reason::WorkerFailed)?
      .map_err(Reason::Identity)?
      .ok_or(Reason::NotSaved)?;
      let mut agent = LocalAgent::start()
        .await
        .map_err(|_| Reason::AgentUnavailable)?;
      let verified = agent
        .add_identity(&snapshot, secret)
        .await
        .map_err(Reason::Identity)?;
      Ok(UnlockedAgent {
        public_key: verified.public_key,
        socket: agent.socket_path().to_owned(),
        _owner: Box::new(agent),
      })
    })
  }
}

pub(super) struct LazyIdentities {
  candidates: Vec<Candidate>,
  context: String,
  unlocker: Arc<dyn Unlocker>,
  fallbacks: Arc<Fallbacks>,
  // A missing entry is unattempted, None is failed/in progress, Some is ready.
  // Holding this lock across unlock serializes and coalesces all clients.
  unlocked: Mutex<HashMap<Vec<u8>, Option<UnlockedAgent>>>,
  cancellation: SyncMutex<Cancellation>,
}

#[derive(Default)]
struct Cancellation {
  closed: bool,
  // The async unlock mutex permits one active read. Previous canceled reads
  // already have their token set, even when their blocking worker outlives it.
  active: Option<Weak<AtomicBool>>,
}

impl LazyIdentities {
  pub(super) fn new(
    candidates: Vec<Candidate>,
    context: String,
    fallbacks: Arc<Fallbacks>,
  ) -> Self {
    Self::with_unlocker_and_fallbacks(candidates, context, Arc::new(KeychainUnlocker), fallbacks)
  }

  #[cfg(test)]
  pub(super) fn with_unlocker(
    candidates: Vec<Candidate>,
    context: String,
    unlocker: Arc<dyn Unlocker>,
  ) -> Self {
    Self::with_unlocker_and_fallbacks(candidates, context, unlocker, Arc::default())
  }

  pub(super) fn with_unlocker_and_fallbacks(
    candidates: Vec<Candidate>,
    context: String,
    unlocker: Arc<dyn Unlocker>,
    fallbacks: Arc<Fallbacks>,
  ) -> Self {
    Self {
      candidates,
      context,
      unlocker,
      fallbacks,
      unlocked: Mutex::new(HashMap::new()),
      cancellation: SyncMutex::default(),
    }
  }

  pub(super) fn cancel(&self) {
    let mut cancellation = self.cancellation.lock().unwrap();
    cancellation.closed = true;
    if let Some(active) = cancellation.active.as_ref().and_then(Weak::upgrade) {
      active.store(true, Ordering::Release);
    }
  }

  pub(super) fn is_canceled(&self) -> bool {
    self.cancellation.lock().unwrap().closed
  }

  pub(super) fn public_identities(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
    self.candidates.iter().map(|candidate| {
      (
        candidate.blob.as_slice(),
        candidate.snapshot.path.as_bytes(),
      )
    })
  }

  pub(super) fn record_failure(&self, key: &[u8], reason: Reason) {
    if !self.is_canceled()
      && let Some(candidate) = self
        .candidates
        .iter()
        .find(|candidate| candidate.blob == key)
    {
      self
        .fallbacks
        .record(&candidate.snapshot.identity_id, reason);
    }
  }

  pub(super) async fn agent_for(&self, key: &[u8]) -> Option<PathBuf> {
    let candidate = self
      .candidates
      .iter()
      .find(|candidate| candidate.blob == key)?;
    let mut unlocked = self.unlocked.lock().await;
    if self.is_canceled() {
      return None;
    }
    if let Some(previous) = unlocked.get(key) {
      return previous.as_ref().map(|agent| agent.socket.clone());
    }
    // Record the attempt before awaiting anything: cancellation, denial, bad
    // passphrases, and changed files must never produce an automatic prompt loop.
    unlocked.insert(key.to_vec(), None);
    let canceled = Arc::new(AtomicBool::new(false));
    {
      let mut cancellation = self.cancellation.lock().unwrap();
      if cancellation.closed {
        return None;
      }
      cancellation.active = Some(Arc::downgrade(&canceled));
    }
    let _cancel_read = CancelRead(Arc::clone(&canceled));
    let result = tokio::time::timeout(
      super::agent_proxy::REQUEST_TIMEOUT,
      self.unlocker.unlock(
        Arc::clone(&candidate.snapshot),
        self.context.clone(),
        Arc::clone(&canceled),
      ),
    )
    .await;
    if canceled.load(Ordering::Acquire) {
      return None;
    }
    let agent = match result
      .map_err(|_| Reason::TimedOut)
      .and_then(std::convert::identity)
    {
      Ok(agent) => agent,
      Err(reason) => {
        self.record_failure(key, reason);
        return None;
      }
    };
    if public_blob(&agent.public_key).as_deref() != Some(key) {
      self.record_failure(
        key,
        Reason::Identity(identities::IdentityError::UnlockFailed),
      );
      return None;
    }
    let socket = agent.socket.clone();
    self.fallbacks.clear(&candidate.snapshot.identity_id);
    unlocked.insert(key.to_vec(), Some(agent));
    Some(socket)
  }
}

pub(super) fn public_blob(public_key: &str) -> Option<Vec<u8>> {
  if public_key.len() > 64 * 1024 || public_key.chars().any(char::is_control) {
    return None;
  }
  let mut fields = public_key.split_whitespace();
  let kind = fields.next()?;
  let blob = base64::engine::general_purpose::STANDARD
    .decode(fields.next()?)
    .ok()?;
  let mut cursor = blob.as_slice();
  (super::agent_proxy::take_string(&mut cursor)? == kind.as_bytes()).then_some(blob)
}
