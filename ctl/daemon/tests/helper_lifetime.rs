#![cfg(unix)]

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use ctl_ipc::{ClientMessage, ServerMessage, SshTarget};
use tokio::process::{Child, Command};

struct Fixture {
  directory: PathBuf,
  executable: PathBuf,
  socket: PathBuf,
  child: Child,
}

impl Fixture {
  fn start(guard: &ctl_core::test_fixtures::ProcessGuard) -> Self {
    let directory =
      Path::new("/tmp").join(format!("ctld-helper-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = directory.join("ctld");
    guard.copy(env!("CARGO_BIN_EXE_ctld"), &executable).unwrap();
    let socket = directory.join("broker.sock");
    let ssh = directory.join("ssh");
    std::fs::write(
      &ssh,
      concat!(
        "#!/bin/sh\n",
        "if [ \"$1\" = -G ]; then\n",
        "  printf 'controlmaster auto\\ncontrolpath %s\\n' \"$CTLD_TEST_MASTER\"\n",
        "  exit 0\n",
        "fi\n",
        "case \" $* \" in *' -O check '*) exit 0;; esac\n",
        "printf '%s\\n' \"$@\" >> \"$CTLD_TEST_FRESH_SSH\"\n",
        "exit 33\n",
      ),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let child = Command::new(&executable)
      .arg("--socket")
      .arg(&socket)
      .env("HOME", &directory)
      .env("PATH", &directory)
      .env("CTLD_TEST_MASTER", directory.join("master.sock"))
      .env("CTLD_TEST_FRESH_SSH", directory.join("fresh-ssh"))
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    Self {
      directory,
      executable,
      socket,
      child,
    }
  }

  async fn connect(&mut self) -> ctl_ipc::Stream {
    tokio::time::timeout(Duration::from_secs(5), async {
      let mut stream = loop {
        if let Ok(stream) = ctl_ipc::connect_existing_at(&self.socket).await {
          break stream;
        }
        assert!(self.child.try_wait().unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(10)).await;
      };
      ctl_ipc::write_frame(
        &mut stream,
        &ClientMessage::Handshake {
          protocol: ctl_ipc::protocol_offer(),
        },
      )
      .await
      .unwrap();
      assert!(matches!(
        ctl_ipc::read_frame(&mut stream).await.unwrap(),
        Some(ServerMessage::HandshakeAccepted { .. })
      ));
      stream
    })
    .await
    .unwrap()
  }

  async fn request(&mut self, request: ClientMessage) -> ServerMessage {
    let mut stream = self.connect().await;
    ctl_ipc::write_frame(&mut stream, &request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), ctl_ipc::read_frame(&mut stream))
      .await
      .unwrap()
      .unwrap()
      .unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = self.child.start_kill();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

fn target(shared: bool) -> SshTarget {
  SshTarget {
    destination: "fixture.invalid".into(),
    ssh_config_alias: None,
    use_ssh_config_master: Some(shared),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: Vec::new(),
  }
}

#[tokio::test]
async fn deleted_helper_blocks_new_authentication_but_preserves_live_connections() {
  let guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let mut fixture = Fixture::start(&guard);
  drop(fixture.connect().await);
  let broker_identity = std::fs::metadata(&fixture.socket).unwrap().ino();
  std::fs::remove_file(&fixture.executable).unwrap();
  for request in [
    ClientMessage::EnsureMaster {
      target: target(false),
    },
    ClientMessage::EnsureMasterQuiet {
      target: target(false),
    },
  ] {
    let ServerMessage::Error { code, message } = fixture.request(request).await else {
      panic!("a stale helper must fail before SSH startup or any prompt");
    };
    assert_eq!(code, "ssh_start_failed");
    assert!(message.contains("ctld helper"), "{message}");
    assert!(message.contains("ctl components restart ctld"), "{message}");
  }
  assert!(!fixture.directory.join("fresh-ssh").exists());
  assert_eq!(
    std::fs::metadata(&fixture.socket).unwrap().ino(),
    broker_identity
  );

  let master = fixture.directory.join("master.sock");
  let _listener = tokio::net::UnixListener::bind(&master).unwrap();
  let master_identity = std::fs::metadata(&master).unwrap().ino();
  for request in [
    ClientMessage::EnsureMaster {
      target: target(true),
    },
    ClientMessage::MasterStatus {
      target: target(true),
    },
  ] {
    assert!(matches!(
      fixture.request(request).await,
      ServerMessage::MasterReady { control_path } if control_path == master
    ));
  }
  assert_eq!(std::fs::metadata(&master).unwrap().ino(), master_identity);
  assert_eq!(
    std::fs::metadata(&fixture.socket).unwrap().ino(),
    broker_identity
  );
  assert!(!fixture.directory.join("fresh-ssh").exists());
  assert!(fixture.child.try_wait().unwrap().is_none());
  fixture.child.kill().await.unwrap();
}

#[tokio::test]
async fn missing_identity_helper_is_rejected_without_starting_an_agent() {
  let missing = Path::new("/tmp").join(format!("missing-ctld-{}", uuid::Uuid::new_v4().simple()));
  assert!(matches!(
    ctld::identities::LocalAgent::start_with_program(&missing).await,
    Err(ctld::identities::IdentityError::UnlockFailed)
  ));
}
