use super::*;
use crate::identities;
use crate::identity_connection::lazy::{Candidate, LazyIdentities, UnlockedAgent, Unlocker};
use base64::Engine as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Fixture {
  directory: PathBuf,
  snapshot: Arc<identities::IdentitySnapshot>,
  public_key: String,
  blob: Vec<u8>,
}

impl Fixture {
  fn new() -> Self {
    let directory =
      PathBuf::from("/tmp").join(format!("ctld-lazy-key-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&directory)
      .unwrap();
    let mut blob = Vec::new();
    push_string(&mut blob, b"ssh-ed25519");
    push_string(&mut blob, &[42; 32]);
    let public_key = format!(
      "ssh-ed25519 {}",
      base64::engine::general_purpose::STANDARD.encode(&blob)
    );
    // Only the public envelope is parsed. No real private key or Keychain item
    // is created; the injected backend represents local unlock verification.
    let mut envelope = b"openssh-key-v1\0".to_vec();
    for field in [b"aes256-ctr".as_slice(), b"bcrypt", b"fixture"] {
      push_string(&mut envelope, field);
    }
    envelope.extend(1_u32.to_be_bytes());
    push_string(&mut envelope, &blob);
    push_string(&mut envelope, b"synthetic encrypted bytes");
    let path = directory.join("identity");
    std::fs::write(
      &path,
      format!(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(envelope)
      ),
    )
    .unwrap();
    let snapshot = Arc::new(identities::inspect_path(path.to_str().unwrap()).unwrap());
    Self {
      directory,
      snapshot,
      public_key,
      blob,
    }
  }

  fn candidate(&self) -> Candidate {
    Candidate::new(Arc::clone(&self.snapshot), self.public_key.clone()).unwrap()
  }

  fn proxy(&self, unlocker: &Arc<MockUnlocker>, existing: Option<&Path>) -> AgentProxy {
    AgentProxy::with_identities(
      LazyIdentities::with_unlocker(
        vec![self.candidate()],
        "synthetic connection".into(),
        unlocker.clone(),
      ),
      existing,
    )
    .unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

struct MockUnlocker {
  calls: AtomicUsize,
  public_key: Option<String>,
  gate: Option<Arc<Semaphore>>,
  logs: Arc<Mutex<Vec<Log>>>,
  sockets: Arc<Mutex<Vec<PathBuf>>>,
  tokens: Mutex<Vec<Arc<AtomicBool>>>,
}

impl MockUnlocker {
  fn new(public_key: Option<String>, gated: bool) -> Arc<Self> {
    Arc::new(Self {
      calls: AtomicUsize::new(0),
      public_key,
      gate: gated.then(|| Arc::new(Semaphore::new(0))),
      logs: Arc::default(),
      sockets: Arc::default(),
      tokens: Mutex::default(),
    })
  }
}

impl Unlocker for MockUnlocker {
  fn unlock(
    &self,
    snapshot: Arc<identities::IdentitySnapshot>,
    _context: String,
    canceled: Arc<AtomicBool>,
  ) -> Pin<Box<dyn Future<Output = Option<UnlockedAgent>> + Send>> {
    self.calls.fetch_add(1, Ordering::SeqCst);
    self.tokens.lock().unwrap().push(Arc::clone(&canceled));
    let public_key = self.public_key.clone();
    let gate = self.gate.clone();
    let logs = Arc::clone(&self.logs);
    let sockets = Arc::clone(&self.sockets);
    Box::pin(async move {
      if let Some(gate) = gate {
        gate.acquire().await.ok()?.forget();
      }
      if canceled.load(Ordering::Acquire) {
        return None;
      }
      identities::ensure_current(&snapshot).ok()?;
      let public_key = public_key?;
      let blob = crate::identity_connection::lazy::public_blob(&public_key)?;
      let agent = FakeAgent::new(&blob, b"lazy signature");
      logs.lock().unwrap().push(Arc::clone(&agent.log));
      sockets.lock().unwrap().push(agent.socket.clone());
      Some(UnlockedAgent {
        public_key,
        socket: agent.socket.clone(),
        _owner: Box::new(agent),
      })
    })
  }
}

fn binding(session: &[u8]) -> Vec<u8> {
  let mut request = vec![EXTENSION];
  for field in [
    b"session-bind@openssh.com".as_slice(),
    b"host key",
    session,
    b"host signature",
  ] {
    push_string(&mut request, field);
  }
  request.push(0);
  request
}

async fn ready(proxy: &AgentProxy, session: &[u8]) -> UnixStream {
  let mut client = UnixStream::connect(proxy.socket_path()).await.unwrap();
  assert_eq!(
    exchange(&mut client, &binding(session)).await.unwrap(),
    [SUCCESS]
  );
  assert!(parse_identities(&exchange(&mut client, &[REQUEST_IDENTITIES]).await.unwrap()).is_some());
  client
}

async fn wait_for_calls(unlocker: &MockUnlocker, count: usize) {
  tokio::time::timeout(Duration::from_secs(2), async {
    while unlocker.calls.load(Ordering::SeqCst) != count {
      tokio::task::yield_now().await;
    }
  })
  .await
  .unwrap();
}

#[tokio::test]
async fn enumeration_and_binding_never_unlock_and_only_an_exact_offered_key_can_unlock() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
  let proxy = fixture.proxy(&unlocker, None);
  let mut client = ready(&proxy, b"session one").await;
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 0);
  let mut malformed = signature_request(&fixture.blob);
  malformed.push(1);
  for request in [
    signature_request(b"unknown key"),
    malformed,
    vec![SIGN_REQUEST],
  ] {
    assert_eq!(exchange(&mut client, &request).await.unwrap(), [FAILURE]);
  }
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 0);
  for _ in 0..2 {
    assert_eq!(
      exchange(&mut client, &signature_request(&fixture.blob))
        .await
        .unwrap()[0],
      SIGN_RESPONSE
    );
  }
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 1);
  let log = unlocker.logs.lock().unwrap()[0].clone();
  assert_eq!(log.lock().unwrap()[0], binding(b"session one"));
  assert_eq!(log.lock().unwrap()[1], [REQUEST_IDENTITIES]);
}

#[tokio::test]
async fn signing_before_enumeration_cannot_read_a_saved_passphrase() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
  let proxy = fixture.proxy(&unlocker, None);
  let mut client = UnixStream::connect(proxy.socket_path()).await.unwrap();
  assert_eq!(
    exchange(&mut client, &signature_request(&fixture.blob))
      .await
      .unwrap(),
    [FAILURE]
  );
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn denied_and_mismatched_unlocks_never_repeat_or_sign() {
  let fixture = Fixture::new();
  let mut other_blob = Vec::new();
  push_string(&mut other_blob, b"ssh-ed25519");
  push_string(&mut other_blob, &[99; 32]);
  let mismatch = format!(
    "ssh-ed25519 {}",
    base64::engine::general_purpose::STANDARD.encode(other_blob)
  );
  for outcome in [None, Some(mismatch)] {
    let unlocker = MockUnlocker::new(outcome, false);
    let proxy = fixture.proxy(&unlocker, None);
    let mut client = ready(&proxy, b"session").await;
    for _ in 0..2 {
      assert_eq!(
        exchange(&mut client, &signature_request(&fixture.blob))
          .await
          .unwrap(),
        [FAILURE]
      );
    }
    assert_eq!(unlocker.calls.load(Ordering::SeqCst), 1);
    for log in unlocker.logs.lock().unwrap().iter() {
      assert!(log.lock().unwrap().is_empty());
    }
    for socket in unlocker.sockets.lock().unwrap().iter() {
      assert!(!socket.exists());
    }
  }
}

#[tokio::test]
async fn existing_agent_is_authoritative_even_when_it_refuses_a_matching_signature() {
  let fixture = Fixture::new();
  for signature in [b"existing signature".as_slice(), b""] {
    let existing = FakeAgent::new(&fixture.blob, signature);
    let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
    let proxy = fixture.proxy(&unlocker, Some(&existing.socket));
    let mut client = ready(&proxy, b"session").await;
    let response = exchange(&mut client, &signature_request(&fixture.blob))
      .await
      .unwrap();
    assert_eq!(
      response[0],
      if signature.is_empty() {
        FAILURE
      } else {
        SIGN_RESPONSE
      }
    );
    assert_eq!(unlocker.calls.load(Ordering::SeqCst), 0);
  }
}

#[tokio::test]
async fn concurrent_clients_unlock_once_and_keep_their_own_binding_connections() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), true);
  let proxy = fixture.proxy(&unlocker, None);
  let mut first = ready(&proxy, b"first session").await;
  let mut second = ready(&proxy, b"second session").await;
  let request = signature_request(&fixture.blob);
  write_message(&mut first, &request).await.unwrap();
  write_message(&mut second, &request).await.unwrap();
  wait_for_calls(&unlocker, 1).await;
  unlocker.gate.as_ref().unwrap().add_permits(1);
  assert_eq!(read_message(&mut first).await.unwrap()[0], SIGN_RESPONSE);
  assert_eq!(read_message(&mut second).await.unwrap()[0], SIGN_RESPONSE);
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 1);
  let log = unlocker.logs.lock().unwrap()[0].clone();
  let log = log.lock().unwrap();
  assert!(log.contains(&binding(b"first session")));
  assert!(log.contains(&binding(b"second session")));
  assert_eq!(
    log
      .iter()
      .filter(|request| request[0] == SIGN_REQUEST)
      .count(),
    2
  );
}

#[tokio::test]
async fn file_replacement_before_signing_cannot_use_the_prepared_identity() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
  let proxy = fixture.proxy(&unlocker, None);
  let mut client = ready(&proxy, b"session").await;
  std::fs::write(&fixture.snapshot.path, b"replaced synthetic key").unwrap();
  assert_eq!(
    exchange(&mut client, &signature_request(&fixture.blob))
      .await
      .unwrap(),
    [FAILURE]
  );
  assert!(unlocker.sockets.lock().unwrap().is_empty());
}

#[tokio::test]
async fn canceling_the_attempt_during_unlock_cannot_publish_an_agent_afterward() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), true);
  let proxy = fixture.proxy(&unlocker, None);
  let mut client = ready(&proxy, b"session").await;
  write_message(&mut client, &signature_request(&fixture.blob))
    .await
    .unwrap();
  wait_for_calls(&unlocker, 1).await;
  drop(proxy);
  // Drop marks the blocking read canceled synchronously, before task teardown.
  assert!(unlocker.tokens.lock().unwrap()[0].load(Ordering::Acquire));
  assert_eq!(
    tokio::time::timeout(Duration::from_secs(2), client.read_u8())
      .await
      .unwrap()
      .unwrap_err()
      .kind(),
    io::ErrorKind::UnexpectedEof
  );
  unlocker.gate.as_ref().unwrap().add_permits(1);
  assert!(unlocker.sockets.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dropping_the_attempt_releases_unlocked_agents() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
  let proxy = fixture.proxy(&unlocker, None);
  let mut client = ready(&proxy, b"session").await;
  assert_eq!(
    exchange(&mut client, &signature_request(&fixture.blob))
      .await
      .unwrap()[0],
    SIGN_RESPONSE
  );
  let socket = unlocker.sockets.lock().unwrap()[0].clone();
  assert!(socket.exists());
  drop(proxy);
  let _ = tokio::time::timeout(Duration::from_secs(2), client.read_u8())
    .await
    .unwrap();
  assert!(!socket.exists());
}

#[tokio::test]
async fn rejected_bindings_disable_later_lazy_signing_without_unlocking() {
  let fixture = Fixture::new();
  for malformed in [true, false] {
    let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
    let proxy = fixture.proxy(&unlocker, None);
    let mut client = ready(&proxy, b"first session").await;
    let mut invalid = binding(b"next session");
    if malformed {
      invalid.pop();
    } else {
      for _ in 1..MAX_BINDINGS {
        assert_eq!(exchange(&mut client, &invalid).await.unwrap(), [SUCCESS]);
      }
    }
    assert_eq!(exchange(&mut client, &invalid).await.unwrap(), [FAILURE]);
    assert_eq!(
      exchange(&mut client, &signature_request(&fixture.blob))
        .await
        .unwrap(),
      [FAILURE]
    );
    assert_eq!(unlocker.calls.load(Ordering::SeqCst), 0);
  }
}

#[tokio::test]
async fn identities_omitted_by_the_response_limit_cannot_trigger_unlock() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
  let registry = LazyIdentities::with_unlocker(
    vec![fixture.candidate()],
    "synthetic connection".into(),
    unlocker.clone(),
  );
  let mut lazy = ClientIdentities::new(Some(Arc::new(registry)));
  let (client, mut server) = UnixStream::pair().unwrap();
  let worker = tokio::spawn(async move {
    assert_eq!(
      read_message(&mut server).await.unwrap(),
      [REQUEST_IDENTITIES]
    );
    let mut response = vec![IDENTITIES_ANSWER];
    response.extend(u32::try_from(MAX_KEYS).unwrap().to_be_bytes());
    for index in 0..MAX_KEYS {
      push_string(&mut response, format!("upstream key {index}").as_bytes());
      push_string(&mut response, b"upstream");
    }
    write_message(&mut server, &response).await.unwrap();
  });
  let mut peers = [Peer {
    stream: Some(client),
    keys: HashSet::new(),
  }];
  let response = list_identities(&mut peers, &mut lazy, &[REQUEST_IDENTITIES]).await;
  assert_eq!(parse_identities(&response).unwrap().len(), MAX_KEYS);
  assert!(!lazy.advertised.contains(&fixture.blob));
  assert_eq!(
    sign(&mut peers, &mut lazy, &signature_request(&fixture.blob)).await,
    [FAILURE]
  );
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 0);
  worker.await.unwrap();
}

#[tokio::test]
async fn failed_upstream_transport_does_not_allow_a_later_saved_key_fallback() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
  let registry = LazyIdentities::with_unlocker(
    vec![fixture.candidate()],
    "synthetic connection".into(),
    unlocker.clone(),
  );
  let mut lazy = ClientIdentities::new(Some(Arc::new(registry)));
  let (client, mut server) = UnixStream::pair().unwrap();
  let key = fixture.blob.clone();
  let worker = tokio::spawn(async move {
    assert_eq!(
      read_message(&mut server).await.unwrap(),
      [REQUEST_IDENTITIES]
    );
    let mut response = vec![IDENTITIES_ANSWER];
    response.extend(1_u32.to_be_bytes());
    push_string(&mut response, &key);
    push_string(&mut response, b"upstream");
    write_message(&mut server, &response).await.unwrap();
    assert_eq!(read_message(&mut server).await.unwrap()[0], SIGN_REQUEST);
    // Close without a signature response, which invalidates Peer's live keys.
  });
  let mut peers = [Peer {
    stream: Some(client),
    keys: HashSet::new(),
  }];
  let _ = list_identities(&mut peers, &mut lazy, &[REQUEST_IDENTITIES]).await;
  for _ in 0..2 {
    assert_eq!(
      sign(&mut peers, &mut lazy, &signature_request(&fixture.blob)).await,
      [FAILURE]
    );
  }
  assert!(peers[0].keys.is_empty());
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 0);
  worker.await.unwrap();
}

#[tokio::test]
async fn canceled_registry_rejects_cached_agents_and_existing_lazy_signing_connections() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
  let registry = Arc::new(LazyIdentities::with_unlocker(
    vec![fixture.candidate()],
    "synthetic connection".into(),
    unlocker.clone(),
  ));
  let mut lazy = ClientIdentities::new(Some(Arc::clone(&registry)));
  assert!(lazy.bind(&binding(b"session")).await);
  assert!(
    lazy
      .sign(&fixture.blob, &signature_request(&fixture.blob))
      .await
      .is_some()
  );
  assert!(registry.agent_for(&fixture.blob).await.is_some());
  registry.cancel();
  assert!(registry.agent_for(&fixture.blob).await.is_none());
  assert!(
    lazy
      .sign(&fixture.blob, &signature_request(&fixture.blob))
      .await
      .is_none()
  );
  let log = unlocker.logs.lock().unwrap()[0].clone();
  assert_eq!(
    log
      .lock()
      .unwrap()
      .iter()
      .filter(|request| request[0] == SIGN_REQUEST)
      .count(),
    1
  );
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_during_an_outstanding_signature_discards_the_late_response() {
  let fixture = Fixture::new();
  let unlocker = MockUnlocker::new(Some(fixture.public_key.clone()), false);
  let registry = Arc::new(LazyIdentities::with_unlocker(
    vec![fixture.candidate()],
    "synthetic connection".into(),
    unlocker.clone(),
  ));
  let mut lazy = ClientIdentities::new(Some(Arc::clone(&registry)));
  let (client, mut server) = UnixStream::pair().unwrap();
  lazy.peers.insert(
    fixture.blob.clone(),
    Peer {
      stream: Some(client),
      keys: HashSet::new(),
    },
  );
  let request = signature_request(&fixture.blob);
  let (response, ()) = tokio::join!(lazy.sign(&fixture.blob, &request), async {
    assert_eq!(read_message(&mut server).await.unwrap(), request);
    registry.cancel();
    let mut response = vec![SIGN_RESPONSE];
    push_string(&mut response, b"signature completed after cancellation");
    write_message(&mut server, &response).await.unwrap();
  },);
  assert!(response.is_none());
  assert_eq!(unlocker.calls.load(Ordering::SeqCst), 0);
}
