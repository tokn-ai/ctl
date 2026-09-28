#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ctld_ipc::{ClientMessage, ServerMessage, VpnStatus};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::time::timeout;

struct Fixture {
  directory: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
    let directory = PathBuf::from("/tmp").join(format!(
      "ctl-vpn-{}-{}-{}",
      std::process::id(),
      NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
      SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    Self {
      directory: directory.canonicalize().unwrap(),
    }
  }

  fn socket(&self) -> PathBuf {
    self.directory.join("ctld.sock")
  }

  fn command(&self, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctl"));
    command
      .args(args)
      .current_dir(&self.directory)
      .env("CTLD_SOCKET_PATH", self.socket())
      .env("CTLD_BIN", self.directory.join("missing-ctld"))
      .env_remove("CTLD_ASKPASS")
      .stdin(Stdio::null())
      .kill_on_drop(true);
    command
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

async fn reply(listener: &UnixListener, response: ServerMessage) -> ClientMessage {
  let (mut stream, _) = listener.accept().await.unwrap();
  assert!(matches!(
    ctld_ipc::read_frame::<_, ClientMessage>(&mut stream).await.unwrap(),
    Some(ClientMessage::Handshake { protocol_version })
      if protocol_version == ctld_ipc::PROTOCOL_VERSION
  ));
  ctld_ipc::write_frame(
    &mut stream,
    &ServerMessage::HandshakeAccepted {
      protocol_version: ctld_ipc::PROTOCOL_VERSION,
    },
  )
  .await
  .unwrap();
  let request = ctld_ipc::read_frame(&mut stream).await.unwrap().unwrap();
  ctld_ipc::write_frame(&mut stream, &response).await.unwrap();
  request
}

async fn exchange(
  fixture: &Fixture,
  listener: &UnixListener,
  args: &[&str],
  response: ServerMessage,
) -> (Output, ClientMessage) {
  let mut command = fixture.command(args);
  let (output, request) = timeout(Duration::from_secs(5), async {
    tokio::join!(command.output(), reply(listener, response))
  })
  .await
  .unwrap();
  (output.unwrap(), request)
}

fn assert_status(output: &Output, expected: &VpnStatus) {
  assert!(output.status.success(), "{output:?}");
  assert_eq!(
    serde_json::from_slice::<VpnStatus>(&output.stdout).unwrap(),
    *expected
  );
  assert!(output.stderr.is_empty(), "{output:?}");
}

#[tokio::test]
async fn start_status_and_stop_use_daemon_ipc_and_print_json() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let ready = VpnStatus {
    endpoint: Some("socks5h://127.0.0.1:43210".into()),
    container_name: Some("ctld-openconnect-test".into()),
    connection_id: None,
    state: ctld_ipc::VpnState::Connected,
    running: true,
  };
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "start", "--env-file", "work.env"],
    ServerMessage::VpnStatus {
      status: ready.clone(),
    },
  )
  .await;
  assert_status(&output, &ready);
  assert!(matches!(
    request,
    ClientMessage::StartVpn { env_file } if env_file == fixture.directory.join("work.env")
  ));

  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "status"],
    ServerMessage::VpnStatus {
      status: ready.clone(),
    },
  )
  .await;
  assert_status(&output, &ready);
  assert!(matches!(request, ClientMessage::VpnStatus));

  let stopped = VpnStatus::default();
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "stop"],
    ServerMessage::VpnStatus {
      status: stopped.clone(),
    },
  )
  .await;
  assert_status(&output, &stopped);
  assert!(matches!(request, ClientMessage::StopVpn));
}

#[tokio::test]
async fn missing_daemon_is_stopped_and_remote_commands_are_rejected() {
  let fixture = Fixture::new();
  for action in ["status", "stop"] {
    let output = fixture.command(&["vpn", action]).output().await.unwrap();
    assert_status(&output, &VpnStatus::default());
    assert!(!fixture.socket().exists());
  }
  for action in ["start", "status", "stop"] {
    let output = fixture
      .command(&["--host", "vpn-host", "vpn", action])
      .output()
      .await
      .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
      String::from_utf8(output.stderr)
        .unwrap()
        .contains("omit --host")
    );
  }
}

#[tokio::test]
async fn start_launches_ctld_when_absent() {
  use std::os::unix::fs::PermissionsExt as _;

  let fixture = Fixture::new();
  let helper = fixture.directory.join("ctld");
  let marker = fixture.directory.join("started");
  std::fs::write(
    &helper,
    format!(
      "#!/bin/sh\nif [ \"$1\" = --protocol-version ]; then\n  printf '%s\\n' {}\nelse\n  touch \"$CTL_VPN_TEST_MARKER\"\nfi\n",
      ctld_ipc::PROTOCOL_VERSION
    ),
  )
  .unwrap();
  std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
  let mut command = fixture.command(&["vpn", "start"]);
  command
    .env("CTLD_BIN", helper)
    .env("CTL_VPN_TEST_MARKER", &marker);
  let stopped = VpnStatus::default();
  let server = async {
    while !marker.exists() {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    reply(
      &listener,
      ServerMessage::VpnStatus {
        status: stopped.clone(),
      },
    )
    .await
  };
  let (output, request) = timeout(Duration::from_secs(5), async {
    tokio::join!(command.output(), server)
  })
  .await
  .unwrap();
  assert_status(&output.unwrap(), &stopped);
  assert!(matches!(
    request,
    ClientMessage::StartVpn { env_file } if env_file == fixture.directory.join(".env")
  ));
}

#[tokio::test]
async fn daemon_errors_are_reported_without_success_json() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "start"],
    ServerMessage::Error {
      code: "vpn_start_failed".into(),
      message: "synthetic startup failure".into(),
    },
  )
  .await;
  assert!(matches!(request, ClientMessage::StartVpn { .. }));
  assert!(!output.status.success());
  assert!(output.stdout.is_empty());
  let stderr = String::from_utf8(output.stderr).unwrap();
  assert!(stderr.contains("vpn_start_failed"));
  assert!(stderr.contains("synthetic startup failure"));
}
