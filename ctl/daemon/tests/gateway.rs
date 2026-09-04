#[cfg(windows)]
use rmux_ipc::windows::Listener;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::UnixListener as Listener;
use tokio::time::timeout;
use uuid::Uuid;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

struct TestPaths {
  root: PathBuf,
  rmux_socket: PathBuf,
}

impl TestPaths {
  fn new() -> Self {
    let token = Uuid::new_v4().simple().to_string();
    let root = std::env::temp_dir().join(format!("ctld-gateway-{}", &token[..12]));
    std::fs::create_dir(&root).expect("create test directory");
    #[cfg(unix)]
    let rmux_socket = root.join("rmux.sock");
    #[cfg(windows)]
    let rmux_socket = PathBuf::from(format!(r"\\.\pipe\ctld-gateway-{token}"));
    Self { root, rmux_socket }
  }
}

impl Drop for TestPaths {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.root);
  }
}

/// The gateway is a byte-transparent, connection-scoped relay. Closing its
/// SSH-side stream drops only the corresponding local attachment; it never
/// owns or removes the local endpoint.
#[tokio::test]
async fn stdio_gateway_relays_bytes_and_leaves_the_local_endpoint_alive() {
  let paths = TestPaths::new();
  let listener = Listener::bind(&paths.rmux_socket).expect("bind fake rmux endpoint");
  let endpoint = tokio::spawn(async move {
    let mut relay = listener.accept().await.expect("accept relay").0;
    let mut payload = [0_u8; 19];
    relay.read_exact(&mut payload).await.expect("read payload");
    relay.write_all(&payload).await.expect("echo payload");
    relay.flush().await.expect("flush payload");

    let mut probe = [0_u8; 1];
    assert_eq!(
      relay.read(&mut probe).await.expect("observe relay close"),
      0
    );
    drop(relay);

    let mut direct = listener.accept().await.expect("accept direct probe").0;
    let mut request = [0_u8; 4];
    direct.read_exact(&mut request).await.expect("read probe");
    assert_eq!(&request, b"ping");
    direct.write_all(b"pong").await.expect("write probe");
  });

  let config = ctld::ConnectConfig::new(paths.rmux_socket.clone());
  let (mut client, gateway) = tokio::io::duplex(1024);
  let (gateway_reader, gateway_writer) = tokio::io::split(gateway);
  let relay =
    tokio::spawn(async move { ctld::connect(gateway_reader, gateway_writer, &config).await });

  let mut preface = vec![0_u8; ctld::SSH_TRANSPORT_PREFACE.len()];
  client
    .read_exact(&mut preface)
    .await
    .expect("read transport preface");
  assert_eq!(preface, ctld::SSH_TRANSPORT_PREFACE);

  let payload = b"\0raw rmux payload\xff\n";
  client.write_all(payload).await.expect("write raw payload");
  client.flush().await.expect("flush raw payload");
  let mut echoed = vec![0; payload.len()];
  client.read_exact(&mut echoed).await.expect("read raw echo");
  assert_eq!(echoed, payload);
  drop(client);

  timeout(TEST_TIMEOUT, relay)
    .await
    .expect("relay exits after SSH channel closes")
    .expect("join relay")
    .expect("relay closes cleanly");

  let mut direct = rmux_ipc::connect_existing_daemon(&paths.rmux_socket)
    .await
    .expect("local endpoint survives relay");
  direct.write_all(b"ping").await.expect("write direct probe");
  let mut response = [0_u8; 4];
  direct
    .read_exact(&mut response)
    .await
    .expect("read direct response");
  assert_eq!(&response, b"pong");

  endpoint.await.expect("join endpoint");
}

#[tokio::test]
async fn missing_local_endpoint_is_reported_without_starting_an_unconfigured_daemon() {
  let paths = TestPaths::new();
  let config = ctld::ConnectConfig::new(paths.rmux_socket.clone());
  let (client, gateway) = tokio::io::duplex(64);
  let (gateway_reader, gateway_writer) = tokio::io::split(gateway);

  let error = ctld::connect(gateway_reader, gateway_writer, &config)
    .await
    .expect_err("missing endpoint must fail");
  assert!(matches!(error, ctld::DaemonError::RmuxUnavailable(_)));
  drop(client);
}
