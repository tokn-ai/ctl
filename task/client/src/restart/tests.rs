use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::UnixListener;

static NEXT: AtomicUsize = AtomicUsize::new(0);
// Avoid a parallel fork inheriting writable helper descriptors before exec.
static SCRIPTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
  root: PathBuf,
  client: Client,
  info: ComponentInfo,
  _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl Fixture {
  async fn new() -> Self {
    let guard = SCRIPTS.lock().await;
    let root = std::env::temp_dir().join(format!(
      "task-lifecycle-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let executable = root.join("helper");
    let info = ComponentInfo {
      build: ctl_component_info::build_info(),
      protocols: vec![
        ProtocolInfo {
          name: "task".into(),
          version: ctl_task_proto::PROTOCOL_VERSION,
        },
        ProtocolInfo {
          name: "task_control".into(),
          version: control::PROTOCOL_VERSION,
        },
      ],
    };
    std::fs::write(&executable, format!("#!/bin/sh\nif [ \"$1\" = --component-info ]; then\nprintf '%s\\n' '{}'\nelse\nprintf '%s\\n' \"$@\" > '{}'\nfi\n", serde_json::to_string(&info).unwrap(), root.join("spawned").display())).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let client = Client::new(root.join("selected.sock")).with_daemon_executable(executable);
    Self {
      root,
      client,
      info,
      _guard: guard,
    }
  }

  fn listener(&self) -> UnixListener {
    UnixListener::bind(&self.client.socket).unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.root);
  }
}

#[tokio::test]
async fn preflight_pins_legacy_owner_without_sending_any_request() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(
      read_frame::<_, control::ClientMessage>(&mut stream)
        .await
        .unwrap()
        .is_none()
    );
  });
  let prepared = fixture.client.preflight_restart().await.unwrap();
  drop(prepared);
  server.await.unwrap();
  assert!(!fixture.root.join("spawned").exists());
}

#[tokio::test]
async fn changed_binary_and_expired_confirmation_do_not_stop_owner() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    for _ in 0..2 {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(
        read_frame::<_, control::ClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    }
  });
  let prepared = fixture.client.preflight_restart().await.unwrap();
  let executable = fixture.client.executable.as_ref().unwrap();
  let contents = std::fs::read_to_string(executable).unwrap();
  std::fs::write(executable, format!("{contents}# changed\n")).unwrap();
  let error = prepared.restart().await.unwrap_err();
  assert_eq!(error.code(), "taskd_binary_changed");
  assert!(!error.may_have_stopped());
  let mut prepared = fixture.client.preflight_restart().await.unwrap();
  prepared.expires_at = Instant::now();
  let error = prepared.restart().await.unwrap_err();
  assert_eq!(error.code(), "taskd_restart_expired");
  assert!(!error.may_have_stopped());
  server.await.unwrap();
}

#[tokio::test]
async fn busy_refusal_does_not_start_replacement_or_report_destructive_transition() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
      read_frame::<_, control::ClientMessage>(&mut stream)
        .await
        .unwrap(),
      Some(control::ClientMessage::RestartDaemon { .. })
    ));
    write_frame(
      &mut stream,
      &control::ServerMessage::Error {
        message: "ctl-taskd has active tasks".into(),
      },
    )
    .await
    .unwrap();
  });
  let error = fixture
    .client
    .preflight_restart()
    .await
    .unwrap()
    .restart()
    .await
    .unwrap_err();
  assert_eq!(error.code(), "taskd_restart_rejected");
  assert!(!error.may_have_stopped());
  assert!(!fixture.root.join("spawned").exists());
  server.await.unwrap();
}

async fn replacement(fixture: &Fixture, valid: bool) -> Result<RestartOutcome, LifecycleError> {
  let listener = fixture.listener();
  let socket = fixture.client.socket.clone();
  let root = fixture.root.clone();
  let mut info = fixture.info.clone();
  if !valid {
    info.build.source_fingerprint = "0".repeat(64);
  }
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
      read_frame::<_, control::ClientMessage>(&mut stream)
        .await
        .unwrap(),
      Some(control::ClientMessage::RestartDaemon { .. })
    ));
    write_frame(
      &mut stream,
      &control::ServerMessage::RestartAccepted {
        data_directory: root.join("original-data"),
        ctmux_socket: root.join("original-ctmux.sock"),
      },
    )
    .await
    .unwrap();
    drop(listener);
    std::fs::remove_file(&socket).unwrap();
    drop(stream);
    timeout(Duration::from_secs(5), async {
      while !root.join("spawned").exists() {
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
    })
    .await
    .unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
      read_frame::<_, control::ClientMessage>(&mut stream)
        .await
        .unwrap(),
      Some(control::ClientMessage::ComponentStatus { .. })
    ));
    write_frame(
      &mut stream,
      &control::ServerMessage::ComponentStatus {
        build: info.build,
        protocol_version: ctl_task_proto::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
  });
  let result = fixture
    .client
    .preflight_restart()
    .await
    .unwrap()
    .restart()
    .await;
  server.await.unwrap();
  let args = std::fs::read_to_string(fixture.root.join("spawned")).unwrap();
  assert!(args.contains(fixture.root.join("original-data").to_str().unwrap()));
  assert!(args.contains(fixture.root.join("original-ctmux.sock").to_str().unwrap()));
  assert!(args.contains(fixture.client.socket.to_str().unwrap()));
  result
}

#[tokio::test]
async fn legacy_restart_preserves_configuration_and_verifies_successor() {
  let fixture = Fixture::new().await;
  assert_eq!(
    replacement(&fixture, true).await.unwrap().after,
    fixture.info
  );
}

#[tokio::test]
async fn unexpected_successor_is_not_reported_as_verified() {
  let fixture = Fixture::new().await;
  let error = replacement(&fixture, false).await.unwrap_err();
  assert_eq!(error.code(), "taskd_restart_verification_failed");
  assert!(error.may_have_stopped());
}

#[tokio::test]
async fn closed_pinned_owner_is_non_destructive() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    let (_stream, _) = listener.accept().await.unwrap();
  });
  let prepared = fixture.client.preflight_restart().await.unwrap();
  server.await.unwrap();
  let error = prepared.restart().await.unwrap_err();
  assert_eq!(error.code(), "taskd_owner_changed");
  assert!(!error.may_have_stopped());
  assert!(!fixture.root.join("spawned").exists());
}
