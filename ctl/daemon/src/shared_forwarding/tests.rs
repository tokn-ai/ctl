use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Notify;
use tokio::time::timeout;

use super::*;

fn target() -> SshTarget {
  SshTarget {
    destination: "work-alias".into(),
    ssh_config_alias: Some("work-alias".into()),
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: Vec::new(),
  }
}

fn forward(port: u16) -> LocalPortForward {
  LocalPortForward {
    forward_id: "owned-forward".into(),
    bind_address: "127.0.0.1".into(),
    local_port: port,
    remote_host: "database.internal".into(),
    remote_port: 5432,
  }
}

async fn start_test_listener<F, Fut>(
  registry: &mut SharedForwardRegistry,
  target: &SshTarget,
  open_channel: F,
) -> LocalPortForward
where
  F: Fn(TcpStream) -> Fut + Send + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  // Transfer the bound socket itself: releasing an ephemeral port and rebinding
  // it lets another parallel test or process claim the port in between.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let forward = forward(listener.local_addr().unwrap().port());
  registry.start_listener((target.clone(), forward.clone()), listener, open_channel);
  forward
}

async fn echo(stream: TcpStream) {
  let (mut read, mut write) = stream.into_split();
  let _ = tokio::io::copy(&mut read, &mut write).await;
}

#[tokio::test]
async fn occupied_ports_fail_without_disturbing_the_existing_listener() {
  let external = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let forward = forward(external.local_addr().unwrap().port());
  let mut registry = SharedForwardRegistry::default();
  assert!(matches!(
    registry.start_with(&target(), &forward, echo).await,
    Err(RequestError::PortForwardFailed(_))
  ));
  assert!(registry.listeners.is_empty());
  assert!(!registry.cancel(&target(), &forward).await);
  let client = TcpStream::connect(external.local_addr().unwrap())
    .await
    .unwrap();
  let (accepted, _) = timeout(Duration::from_secs(1), external.accept())
    .await
    .unwrap()
    .unwrap();
  assert_eq!(client.local_addr().unwrap(), accepted.peer_addr().unwrap());
}

#[tokio::test]
async fn each_connection_has_its_own_channel_and_cancellation_closes_owned_clients() {
  let target = target();
  let count = Arc::new(AtomicUsize::new(0));
  let mut registry = SharedForwardRegistry::default();
  let opened = Arc::clone(&count);
  let forward = start_test_listener(&mut registry, &target, move |stream| {
    opened.fetch_add(1, Ordering::SeqCst);
    echo(stream)
  })
  .await;

  // Starting the same owner/spec again is idempotent.
  registry.start_with(&target, &forward, echo).await.unwrap();
  let address = (forward.bind_address.as_str(), forward.local_port);
  let mut first = TcpStream::connect(address).await.unwrap();
  let mut second = TcpStream::connect(address).await.unwrap();
  for client in [&mut first, &mut second] {
    client.write_all(b"round trip").await.unwrap();
    let mut response = [0; 10];
    timeout(Duration::from_secs(1), client.read_exact(&mut response))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(&response, b"round trip");
  }
  assert_eq!(count.load(Ordering::SeqCst), 2);
  assert!(registry.cancel(&target, &forward).await);
  for client in [&mut first, &mut second] {
    assert_eq!(
      timeout(Duration::from_secs(1), client.read(&mut [0; 1]))
        .await
        .unwrap()
        .unwrap(),
      0
    );
  }
  assert!(registry.listeners.is_empty());
}

#[tokio::test]
async fn cancellation_requires_both_the_owning_target_and_exact_forward_specification() {
  let target = target();
  let mut registry = SharedForwardRegistry::default();
  let forward = start_test_listener(&mut registry, &target, echo).await;
  let other_target = SshTarget {
    destination: "other-alias".into(),
    ..target.clone()
  };
  let other_forward = LocalPortForward {
    remote_port: 5433,
    ..forward.clone()
  };
  assert!(!registry.cancel(&other_target, &forward).await);
  assert!(!registry.cancel(&target, &other_forward).await);
  assert_eq!(registry.listeners.len(), 1);
  assert!(registry.cancel(&target, &forward).await);
}

#[tokio::test]
async fn dropping_registry_cancels_live_channel_tasks() {
  struct Completion(Arc<Notify>);
  impl Drop for Completion {
    fn drop(&mut self) {
      self.0.notify_one();
    }
  }
  let target = target();
  let connected = Arc::new(Notify::new());
  let stopped = Arc::new(Notify::new());
  let channel_connected = Arc::clone(&connected);
  let channel_stopped = Arc::clone(&stopped);
  let mut registry = SharedForwardRegistry::default();
  let forward = start_test_listener(&mut registry, &target, move |stream| {
    let connected = Arc::clone(&channel_connected);
    let stopped = Arc::clone(&channel_stopped);
    async move {
      let _completion = Completion(stopped);
      connected.notify_one();
      echo(stream).await;
    }
  })
  .await;
  let _client = TcpStream::connect((forward.bind_address.as_str(), forward.local_port))
    .await
    .unwrap();
  timeout(Duration::from_secs(1), connected.notified())
    .await
    .unwrap();
  drop(registry);
  timeout(Duration::from_secs(1), stopped.notified())
    .await
    .unwrap();
}

#[tokio::test]
async fn listener_validation_prevents_implicit_or_public_bind_addresses() {
  let mut registry = SharedForwardRegistry::default();
  for bind in ["localhost", "0.0.0.0", "::", ""] {
    let forward = LocalPortForward {
      bind_address: bind.into(),
      ..forward(5432)
    };
    assert!(matches!(
      registry.start_with(&target(), &forward, echo).await,
      Err(RequestError::InvalidRequest(_))
    ));
  }
  assert!(registry.listeners.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn tcp_half_close_reaches_the_channel_child_and_drains_its_reply() {
  let target = target();
  let mut registry = SharedForwardRegistry::default();
  let forward = start_test_listener(&mut registry, &target, |stream| async move {
    let mut command = Command::new("cat");
    command
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .kill_on_drop(true);
    relay_channel(stream, command).await.unwrap();
  })
  .await;
  let mut client = TcpStream::connect((forward.bind_address.as_str(), forward.local_port))
    .await
    .unwrap();
  client.write_all(b"request then EOF").await.unwrap();
  client.shutdown().await.unwrap();
  let mut response = Vec::new();
  timeout(Duration::from_secs(2), client.read_to_end(&mut response))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(response, b"request then EOF");
  assert!(registry.cancel(&target, &forward).await);
}

#[test]
fn channels_use_strict_mux_stdio_forwarding_without_changing_master_listeners() {
  let forward = LocalPortForward {
    remote_host: "fd7a:115c:a1e0::1".into(),
    ..forward(15432)
  };
  let command = channel_command(&target(), Path::new("/tmp/shared control.sock"), &forward);
  let arguments = command
    .as_std()
    .get_args()
    .map(|arg| arg.to_str().unwrap())
    .collect::<Vec<_>>();
  assert_eq!(&arguments[..3], &["-S", "/tmp/shared control.sock", "-T"]);
  for option in [
    "ControlMaster=no",
    "ProxyCommand=false",
    "BatchMode=yes",
    "StdinNull=no",
    "ForkAfterAuthentication=no",
    "ClearAllForwardings=yes",
    "PermitLocalCommand=no",
  ] {
    assert!(arguments.windows(2).any(|args| args == ["-o", option]));
  }
  assert!(
    arguments
      .windows(2)
      .any(|args| args == ["-W", "[fd7a:115c:a1e0::1]:5432"])
  );
  assert!(!arguments.contains(&"-O"));
  assert!(!arguments.contains(&"-L"));
  assert_eq!(&arguments[arguments.len() - 2..], &["--", "work-alias"]);
}
