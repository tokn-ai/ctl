#![cfg(windows)]

use ctl_ipc::lifecycle::{Client, DaemonStatus, Request, Response};
use ctl_ipc::{ClientMessage, ServerMessage, SshTarget};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::time::{Duration, sleep, timeout};

const DEADLINE: Duration = Duration::from_secs(10);

struct Owner {
  socket: PathBuf,
  directory: PathBuf,
  child: tokio::process::Child,
}

impl Owner {
  async fn start(socket: Option<PathBuf>) -> Self {
    let id = uuid::Uuid::new_v4();
    let directory = std::env::temp_dir().join(format!("ctld-windows-{id}"));
    std::fs::create_dir(&directory).unwrap();
    let socket = socket.unwrap_or_else(|| PathBuf::from(format!(r"\\.\pipe\ctld-test-{id}")));
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctld"))
      .arg("--socket")
      .arg(&socket)
      .arg("--detach-from-terminal")
      .creation_flags(0x0000_0008)
      .env("PATH", &directory)
      .env("USERPROFILE", &directory)
      .env("LOCALAPPDATA", &directory)
      .env("APPDATA", &directory)
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let owner = Self {
      socket,
      directory,
      child,
    };
    timeout(DEADLINE, async {
      loop {
        if matches!(owner.info().await, DaemonStatus::Running { .. }) {
          break;
        }
        sleep(Duration::from_millis(25)).await;
      }
    })
    .await
    .expect("isolated detached Windows ctld did not start");
    owner
  }

  async fn info(&self) -> DaemonStatus {
    Client::new(self.socket.clone()).probe().await.unwrap()
  }

  async fn connect(&self) -> ctl_ipc::Stream {
    let mut stream = ctl_ipc::connect_existing_at(&self.socket).await.unwrap();
    ctl_ipc::write_frame(
      &mut stream,
      &ClientMessage::Handshake {
        protocol: ctl_ipc::protocol_offer(),
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
        .await
        .unwrap(),
      Some(ServerMessage::HandshakeAccepted { protocol_version })
        if protocol_version == ctl_ipc::PROTOCOL_VERSION
    ));
    stream
  }

  async fn stop(&mut self) {
    let mut stream = ctl_ipc::connect_existing_at(&self.socket).await.unwrap();
    ctl_ipc::write_frame(
      &mut stream,
      &Request::CtldInspect {
        protocol: ctl_ipc::lifecycle::protocol_offer(),
      },
    )
    .await
    .unwrap();
    let Some(Response::CtldInfo { info, .. }) = ctl_ipc::read_frame(&mut stream).await.unwrap()
    else {
      panic!("missing owner identity");
    };
    ctl_ipc::write_frame(
      &mut stream,
      &Request::CtldRestart {
        expected_instance_id: info.instance_id.clone(),
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      ctl_ipc::read_frame::<_, Response>(&mut stream)
        .await
        .unwrap(),
      Some(Response::CtldRestartAccepted { instance_id }) if instance_id == info.instance_id
    ));
    assert!(
      timeout(DEADLINE, ctl_ipc::read_frame::<_, Response>(&mut stream))
        .await
        .unwrap()
        .unwrap()
        .is_none()
    );
    assert!(
      timeout(DEADLINE, self.child.wait())
        .await
        .unwrap()
        .unwrap()
        .success()
    );
    assert_eq!(self.info().await, DaemonStatus::Absent);
  }
}

impl Drop for Owner {
  fn drop(&mut self) {
    let _ = self.child.start_kill();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

#[tokio::test]
async fn detached_owner_serves_vpn_status_without_a_container_engine_and_releases_its_pipe() {
  timeout(DEADLINE * 4, async {
    let mut owner = Owner::start(None).await;
    assert!(matches!(
      ctld::run(owner.socket.clone()).await,
      Err(ctld::DaemonError::Bind { .. })
    ));
    let DaemonStatus::Running { info: before } = owner.info().await else {
      panic!("owner disappeared");
    };
    assert_eq!(before.active_vpn_count, 0);
    assert_eq!(
      before.binary,
      ctl_ipc::lifecycle::DaemonBinaryInfo::current()
    );
    let inventory = ctl_ipc::vpn::Client::new(owner.socket.clone())
      .list()
      .await
      .unwrap();
    assert!(inventory.supports_multiple);
    assert!(inventory.connections.is_empty());
    assert!(!inventory.discovery_warnings.is_empty());
    let mut stream = owner.connect().await;
    // A missing optional container engine leaves the broker usable and returns
    // discovery diagnostics. Current contracts permit repeated status requests.
    for _ in 0..2 {
      ctl_ipc::write_frame(&mut stream, &ClientMessage::VpnStatus)
        .await
        .unwrap();
      let Some(ServerMessage::VpnStatus {
        status,
        snapshot: Some(snapshot),
      }) = ctl_ipc::read_frame(&mut stream).await.unwrap()
      else {
        panic!("VPN status unavailable");
      };
      assert!(!status.running);
      assert!(snapshot.connections.is_empty());
      assert!(!snapshot.discovery_warnings.is_empty());
    }
    drop(stream);
    owner.stop().await;
    let mut replacement = Owner::start(Some(owner.socket.clone())).await;
    let DaemonStatus::Running { info: after } = replacement.info().await else {
      panic!("replacement disappeared");
    };
    assert_ne!(before.instance_id, after.instance_id);
    replacement.stop().await;
  })
  .await
  .unwrap();
}

#[tokio::test]
async fn multiplexing_requests_report_the_platform_limit_without_stopping_the_owner() {
  timeout(DEADLINE * 3, async {
    let mut owner = Owner::start(None).await;
    let mut stream = owner.connect().await;
    ctl_ipc::write_frame(
      &mut stream,
      &ClientMessage::EnsureMaster {
        target: SshTarget {
          destination: "fixture.example.test".into(),
          hostname: None,
          user: None,
          port: None,
          identity_file: None,
          ssh_config_alias: None,
          use_ssh_config_master: None,
          gateways: Vec::new(),
        },
      },
    )
    .await
    .unwrap();
    let Some(ServerMessage::Error { code, message }) =
      ctl_ipc::read_frame(&mut stream).await.unwrap()
    else {
      panic!("missing platform diagnostic");
    };
    assert_eq!(code, "ctld_platform_unsupported");
    assert!(message.contains("direct SSH"));
    assert!(matches!(owner.info().await, DaemonStatus::Running { .. }));
    owner.stop().await;
  })
  .await
  .unwrap();
}

#[tokio::test]
async fn native_identity_helper_lists_files_while_keychain_is_unavailable() {
  use ctl_ipc::identities::{FileState, PassphraseState, Request as IdentityRequest};
  use tokio::io::AsyncWriteExt as _;

  let directory = std::env::temp_dir().join(format!("ctld-identity-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir(&directory).unwrap();
  let path = directory.join("identity with spaces");
  std::fs::write(
    &path,
    "-----BEGIN PRIVATE KEY-----\nMAsCAQAwAwYBKgQBeA==\n-----END PRIVATE KEY-----\n",
  )
  .unwrap();
  let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--identity-request")
    .env("USERPROFILE", &directory)
    .env("LOCALAPPDATA", &directory)
    .env("APPDATA", &directory)
    .env_remove("CTLD_ASKPASS")
    .env_remove("CTLD_IDENTITY_ASKPASS")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let mut input = child.stdin.take().unwrap();
  input
    .write_all(
      &serde_json::to_vec(&IdentityRequest::ListMetadata {
        paths: vec![path.to_string_lossy().into_owned()],
      })
      .unwrap(),
    )
    .await
    .unwrap();
  drop(input);
  let output = timeout(DEADLINE, child.wait_with_output())
    .await
    .unwrap()
    .unwrap();
  std::fs::remove_dir_all(directory).unwrap();
  assert!(output.status.success());
  assert!(output.stderr.is_empty());
  let ctl_ipc::identities::Response::Inventory { inventory } =
    serde_json::from_slice(&output.stdout).unwrap()
  else {
    panic!("missing identity inventory");
  };
  assert!(!inventory.keychain_available);
  assert!(inventory.file_discovery_complete);
  assert_eq!(inventory.identity_files.len(), 1);
  assert_eq!(inventory.identity_files[0].file_state, FileState::Ready);
  assert_eq!(
    inventory.identity_files[0].passphrase_state,
    PassphraseState::NotRequired
  );
}
