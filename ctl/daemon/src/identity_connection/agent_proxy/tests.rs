use super::*;
use std::sync::Mutex;

type Log = Arc<Mutex<Vec<Vec<u8>>>>;

struct FakeAgent {
  directory: PathBuf,
  socket: PathBuf,
  worker: JoinHandle<()>,
  log: Log,
}

impl FakeAgent {
  fn new(key: &[u8], signature: &[u8]) -> Self {
    let directory =
      PathBuf::from("/tmp").join(format!("ctld-fake-agent-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&directory)
      .unwrap();
    let socket = directory.join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&log);
    let key = key.to_vec();
    let signature = signature.to_vec();
    let worker = tokio::spawn(async move {
      let mut clients = JoinSet::new();
      loop {
        tokio::select! {
          connection = listener.accept() => {
            let Ok((mut stream, _)) = connection else { break };
            let recorded = Arc::clone(&recorded);
            let key = key.clone();
            let signature = signature.clone();
            clients.spawn(async move {
              let mut bound = false;
              while let Ok(request) = read_message(&mut stream).await {
                recorded.lock().unwrap().push(request.clone());
                let response = match request[0] {
                  EXTENSION => { bound = true; vec![SUCCESS] }
                  REQUEST_IDENTITIES => {
                    let mut response = vec![IDENTITIES_ANSWER];
                    response.extend(1_u32.to_be_bytes());
                    push_string(&mut response, &key);
                    push_string(&mut response, b"synthetic public identity");
                    response
                  }
                  SIGN_REQUEST if bound && !signature.is_empty() => {
                    let mut response = vec![SIGN_RESPONSE];
                    push_string(&mut response, &signature);
                    response
                  }
                  _ => vec![FAILURE],
                };
                if write_message(&mut stream, &response).await.is_err() { break; }
              }
            });
          }
          _ = clients.join_next(), if !clients.is_empty() => {}
        }
      }
    });
    Self {
      directory,
      socket,
      worker,
      log,
    }
  }
}

impl Drop for FakeAgent {
  fn drop(&mut self) {
    self.worker.abort();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

async fn bind(stream: &mut UnixStream) {
  let mut request = vec![EXTENSION];
  push_string(&mut request, b"session-bind@openssh.com");
  assert_eq!(exchange(stream, &request).await.unwrap(), [SUCCESS]);
}

fn signature_request(key: &[u8]) -> Vec<u8> {
  let mut request = vec![SIGN_REQUEST];
  push_string(&mut request, key);
  push_string(&mut request, b"authentication exchange");
  request.extend(0_u32.to_be_bytes());
  request
}

#[tokio::test]
async fn merges_existing_and_local_agents_with_binding_and_signing_on_the_same_connections() {
  let existing = FakeAgent::new(b"existing key", b"existing signature");
  let local = FakeAgent::new(b"verified local key", b"local signature");
  let proxy = AgentProxy::start(&local.socket, Some(&existing.socket)).unwrap();
  let mut client = UnixStream::connect(proxy.socket_path()).await.unwrap();
  bind(&mut client).await;
  let response = exchange(&mut client, &[REQUEST_IDENTITIES]).await.unwrap();
  let identities = parse_identities(&response).unwrap();
  assert_eq!(
    identities
      .iter()
      .map(|(key, _)| key.as_slice())
      .collect::<Vec<_>>(),
    [b"existing key".as_slice(), b"verified local key".as_slice()]
  );
  for (key, expected) in [
    (b"existing key".as_slice(), b"existing signature".as_slice()),
    (
      b"verified local key".as_slice(),
      b"local signature".as_slice(),
    ),
  ] {
    let response = exchange(&mut client, &signature_request(key))
      .await
      .unwrap();
    assert_eq!(response[0], SIGN_RESPONSE);
    assert_eq!(take_string(&mut &response[1..]), Some(expected));
  }
  // The existing agent receives only its own signature request.
  assert_eq!(
    existing
      .log
      .lock()
      .unwrap()
      .iter()
      .filter(|request| request[0] == SIGN_REQUEST)
      .count(),
    1
  );
  assert_eq!(
    local
      .log
      .lock()
      .unwrap()
      .iter()
      .filter(|request| request[0] == SIGN_REQUEST)
      .count(),
    1
  );
}

#[tokio::test]
async fn rejects_agent_mutations_and_unknown_extensions_without_forwarding_them() {
  let existing = FakeAgent::new(b"existing key", b"signature");
  let local = FakeAgent::new(b"local key", b"signature");
  let proxy = AgentProxy::start(&local.socket, Some(&existing.socket)).unwrap();
  let mut client = UnixStream::connect(proxy.socket_path()).await.unwrap();
  for request in [vec![17], vec![18], vec![19], vec![20], vec![22], vec![23]] {
    assert_eq!(exchange(&mut client, &request).await.unwrap(), [FAILURE]);
  }
  let mut extension = vec![EXTENSION];
  push_string(&mut extension, b"untrusted extension");
  assert_eq!(
    exchange(&mut client, &extension).await.unwrap(),
    [EXTENSION_FAILURE]
  );
  assert!(existing.log.lock().unwrap().is_empty());
  assert!(local.log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_or_failed_local_agent_preserves_existing_agent_fallback() {
  let existing = FakeAgent::new(b"existing key", b"signature");
  let missing = existing.directory.join("missing");
  let proxy = AgentProxy::start(&missing, Some(&existing.socket)).unwrap();
  let mut client = UnixStream::connect(proxy.socket_path()).await.unwrap();
  bind(&mut client).await;
  let response = exchange(&mut client, &[REQUEST_IDENTITIES]).await.unwrap();
  assert_eq!(parse_identities(&response).unwrap().len(), 1);
  assert_eq!(
    exchange(&mut client, &signature_request(b"existing key"))
      .await
      .unwrap()[0],
    SIGN_RESPONSE
  );
}

#[tokio::test]
async fn dropping_the_attempt_removes_public_hints_socket_and_active_proxy_clients() {
  let local = FakeAgent::new(b"key", b"signature");
  let proxy = AgentProxy::start(&local.socket, None).unwrap();
  let directory = proxy.directory.clone();
  let public_file = proxy.write_public_key("ssh-ed25519 cHVibGlj", 0).unwrap();
  assert!(public_file.exists());
  let mut client = UnixStream::connect(proxy.socket_path()).await.unwrap();
  let _ = exchange(&mut client, &[REQUEST_IDENTITIES]).await.unwrap();
  drop(proxy);
  assert!(!directory.exists());
  let mut byte = [0];
  assert_eq!(
    tokio::time::timeout(Duration::from_secs(1), client.read(&mut byte))
      .await
      .unwrap()
      .unwrap(),
    0
  );
}

#[test]
fn malformed_identity_lists_and_strings_are_rejected_without_allocating_unbounded_payloads() {
  assert!(parse_identities(&[IDENTITIES_ANSWER, 255, 255, 255, 255]).is_none());
  assert!(parse_identities(&[IDENTITIES_ANSWER, 0, 0, 0, 1]).is_none());
  assert!(parse_identities(&[IDENTITIES_ANSWER, 0, 0, 0, 0, 1]).is_none());
  assert!(take_string(&mut [255, 255, 255, 255].as_slice()).is_none());
}

#[tokio::test]
async fn a_failed_upstream_exchange_closes_that_stream_before_any_later_request() {
  let (client, mut server) = UnixStream::pair().unwrap();
  let worker = tokio::spawn(async move {
    assert_eq!(
      read_message(&mut server).await.unwrap(),
      [REQUEST_IDENTITIES]
    );
    server
      .write_u32(u32::try_from(MAX_MESSAGE + 1).unwrap())
      .await
      .unwrap();
    let mut byte = [0];
    assert_eq!(server.read(&mut byte).await.unwrap(), 0);
  });
  let mut peer = Peer {
    stream: Some(client),
    keys: HashSet::from([b"stale key".to_vec()]),
  };
  assert!(peer.request(&[REQUEST_IDENTITIES]).await.is_err());
  assert!(peer.stream.is_none());
  assert!(peer.keys.is_empty());
  assert!(peer.request(&[REQUEST_IDENTITIES]).await.is_err());
  worker.await.unwrap();
}

mod lazy;
