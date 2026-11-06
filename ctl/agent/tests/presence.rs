#![cfg(unix)]

use ctl_core::presence::{ClientIdentity, ServiceConnection, connections_in};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    // A short path also fits the macOS Unix socket address limit.
    let id = uuid::Uuid::new_v4().simple().to_string();
    let path = std::env::temp_dir().join(format!("ctl-p-{}", &id[..12]));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&path)
      .unwrap();
    Self(path)
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn client_identity() -> ClientIdentity {
  ClientIdentity {
    instance_id: "fixture-client".into(),
    client_name: "ctmux-app".into(),
    client_version: "1.0".into(),
    device_name: Some("Fixture device".into()),
  }
}

fn gateway(home: &Path, arguments: &[&str]) -> Child {
  Command::new(env!("CARGO_BIN_EXE_ctl-agent"))
    .args(arguments)
    .env("HOME", home)
    .env("CTMUX_RUNTIME_DIR", home)
    .env("XDG_RUNTIME_DIR", home)
    .env("SSH_CONNECTION", "192.0.2.1 49152 192.0.2.2 22")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap()
}

async fn open_channel(
  home: &Path,
  listener: &UnixListener,
  client: &ClientIdentity,
) -> (Child, UnixStream) {
  let mut child = gateway(home, &["connect", "--identity"]);
  let stdout = child.stdout.as_mut().unwrap();
  let mut marker = vec![0; ctl_proto::IDENTITY_PREFACE.len()];
  stdout.read_exact(&mut marker).await.unwrap();
  assert_eq!(marker, ctl_proto::IDENTITY_PREFACE);
  let version = ctl_proto::negotiate_identity_contract_with_client(
    stdout,
    child.stdin.as_mut().unwrap(),
    Some(client),
  )
  .await
  .unwrap();
  assert_eq!(version, ctl_proto::IDENTITY_PROTOCOL_VERSION);
  let daemon = listener.accept().await.unwrap().0;
  let identity = ctl_proto::read_identity(stdout).await.unwrap();
  assert!(identity.is_valid());
  (child, daemon)
}

#[tokio::test]
async fn incoming_channels_are_visible_individually_until_eof_or_gateway_death() {
  let home = Home::new();
  let listener = UnixListener::bind(home.0.join("ctmux.sock")).unwrap();
  let directory = home.0.join(".tokn/ctl/connections");
  let client = client_identity();
  tokio::time::timeout(Duration::from_secs(15), async {
    let (mut first, _first_daemon) = open_channel(&home.0, &listener, &client).await;
    let initial = connections_in(&directory).unwrap();
    assert_eq!(initial.len(), 1);
    let (mut second, _second_daemon) = open_channel(&home.0, &listener, &client).await;
    let both = connections_in(&directory).unwrap();
    assert_eq!(both.len(), 2);
    assert_ne!(both[0].connection_id, both[1].connection_id);
    for channel in &both {
      assert_eq!(channel.client.as_ref(), Some(&client));
      assert_eq!(channel.service, "ctmux");
      assert_eq!(channel.source.as_deref(), Some("192.0.2.1"));
      assert!(channel.connected_at_ms > 0);
    }

    drop(first.stdin.take());
    assert!(first.wait().await.unwrap().success());
    let remaining = connections_in(&directory).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_ne!(remaining[0].connection_id, initial[0].connection_id);

    // SIGKILL bypasses the gateway's destructor. The query has to recognize
    // the released OS lease and clean up the residual on-disk record.
    second.kill().await.unwrap();
    assert_eq!(
      connections_in(&directory).unwrap(),
      Vec::<ServiceConnection>::new()
    );
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
  })
  .await
  .expect("gateway registration and teardown must complete");
}

#[tokio::test]
async fn tracking_failure_preserves_the_service_stream_and_reports_one_diagnostic() {
  let home = Home::new();
  let listener = UnixListener::bind(home.0.join("ctmux.sock")).unwrap();
  let account = home.0.join(".tokn/ctl");
  std::fs::create_dir_all(&account).unwrap();
  let registry = account.join("connections");
  std::fs::write(&registry, b"existing file").unwrap();

  tokio::time::timeout(Duration::from_secs(15), async {
    let (mut child, mut daemon) = open_channel(&home.0, &listener, &client_identity()).await;
    let mut data = [0; 4];
    child
      .stdin
      .as_mut()
      .unwrap()
      .write_all(b"ping")
      .await
      .unwrap();
    daemon.read_exact(&mut data).await.unwrap();
    assert_eq!(&data, b"ping");
    daemon.write_all(b"pong").await.unwrap();
    child
      .stdout
      .as_mut()
      .unwrap()
      .read_exact(&mut data)
      .await
      .unwrap();
    assert_eq!(&data, b"pong");

    drop(child.stdin.take());
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
    let diagnostic = std::str::from_utf8(&output.stderr).unwrap();
    assert_eq!(diagnostic.lines().count(), 1);
    assert!(diagnostic.contains("live connection tracking unavailable"));
    assert!(diagnostic.len() <= 256);
    assert_eq!(std::fs::read(&registry).unwrap(), b"existing file");
  })
  .await
  .expect("tracking failure must allow the service stream to finish");
}

#[tokio::test]
async fn vpn_channel_registers_its_negotiated_client_and_expires_at_eof() {
  use ctl_core::protocol::ProtocolOffer;
  use ctl_ipc::remote_vpn::{CONTRACT_V1_1_2, PREFACE, ProtocolSelection};

  let home = Home::new();
  let registry = home.0.join(".tokn/ctl/connections");
  tokio::time::timeout(Duration::from_secs(15), async {
    let client = client_identity();
    let mut child = gateway(&home.0, &["vpn"]);
    let stdout = child.stdout.as_mut().unwrap();
    let mut marker = vec![0; PREFACE.len()];
    stdout.read_exact(&mut marker).await.unwrap();
    assert_eq!(marker, PREFACE);
    let offer: ProtocolOffer = ctl_ipc::read_frame(stdout).await.unwrap().unwrap();
    assert!(offer.accepts(CONTRACT_V1_1_2));
    ctl_ipc::write_frame(
      child.stdin.as_mut().unwrap(),
      &ProtocolSelection {
        protocol_version: CONTRACT_V1_1_2,
        client_identity: Some(client.clone()),
      },
    )
    .await
    .unwrap();
    assert!(ctl_proto::read_identity(stdout).await.unwrap().is_valid());
    let rows = connections_in(&registry).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].client.as_ref(), Some(&client));
    assert_eq!(rows[0].service, "vpn");
    assert_eq!(rows[0].source.as_deref(), Some("192.0.2.1"));

    // EOF before the first VPN operation closes the gateway without contacting ctld.
    drop(child.stdin.take());
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(output.stderr, Vec::<u8>::new());
    assert_eq!(
      connections_in(&registry).unwrap(),
      Vec::<ServiceConnection>::new()
    );
  })
  .await
  .expect("VPN identity and EOF must complete");
}
