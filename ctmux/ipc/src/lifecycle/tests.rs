use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::UnixListener;

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
  root: PathBuf,
  client: Client,
  info: ComponentInfo,
  _guard: ctl_core::test_fixtures::ProcessGuard,
}

impl Fixture {
  async fn new() -> Self {
    let guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let root = std::env::temp_dir().join(format!(
      "ctmux-lifecycle-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let executable = root.join("helper");
    let info = ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![
        ctmux_proto::protocol_info(),
        crate::local_control_protocol_info(),
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
    UnixListener::bind(crate::control_socket_path(&self.client.socket).unwrap()).unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.root);
  }
}

async fn handshake_reply(stream: &mut Stream, info: Option<&ComponentInfo>) {
  assert!(matches!(
    crate::read_local_control_frame::<_, LocalControlClientMessage>(stream)
      .await
      .unwrap(),
    Some(LocalControlClientMessage::Handshake { .. })
  ));
  crate::write_local_control_frame(
    stream,
    &LocalControlServerMessage::HandshakeAccepted {
      protocol_version: crate::LOCAL_CONTROL_PROTOCOL_VERSION,
      protocols: info.map_or_else(
        || vec![crate::local_control_protocol_info()],
        |info| info.protocols.clone(),
      ),
      restart_supported: true,
      managed_sessions_supported: false,
      build: info.map(|info| info.build.clone()),
      data_protocol_version: info.map(|_| ctmux_proto::PROTOCOL_VERSION),
    },
  )
  .await
  .unwrap();
}

#[tokio::test]
async fn legacy_preflight_does_not_send_restart_and_pins_unknown_build() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    handshake_reply(&mut stream, None).await;
    assert!(
      crate::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
        .await
        .unwrap()
        .is_none()
    );
  });
  let prepared = fixture.client.preflight_restart().await.unwrap();
  assert!(prepared.before.build.is_none());
  drop(prepared);
  server.await.unwrap();
  assert!(!fixture.root.join("spawned").exists());
}

#[tokio::test]
async fn changed_executable_and_expired_confirmation_preserve_owner() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    for _ in 0..2 {
      let (mut stream, _) = listener.accept().await.unwrap();
      handshake_reply(&mut stream, None).await;
      assert!(
        crate::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    }
  });
  let prepared = fixture.client.preflight_restart().await.unwrap();
  let executable = fixture.client.executable.as_ref().unwrap();
  let old = std::fs::read_to_string(executable).unwrap();
  std::fs::write(executable, format!("{old}# changed\n")).unwrap();
  let error = prepared.restart().await.unwrap_err();
  assert_eq!(error.code(), "ctmuxd_binary_changed");
  assert!(!error.may_have_stopped());
  let mut prepared = fixture.client.preflight_restart().await.unwrap();
  prepared.expires_at = Instant::now();
  let error = prepared.restart().await.unwrap_err();
  assert_eq!(error.code(), "ctmuxd_restart_expired");
  assert!(!error.may_have_stopped());
  server.await.unwrap();
}

async fn replacement(
  fixture: &Fixture,
  valid: bool,
  numeric: bool,
) -> Result<RestartOutcome, LifecycleError> {
  let listener = fixture.listener();
  let control = crate::control_socket_path(&fixture.client.socket).unwrap();
  let socket = fixture.client.socket.clone();
  let spawned = fixture.root.join("spawned");
  let mut info = fixture.info.clone();
  if !valid {
    info.build.source_fingerprint = "0".repeat(64);
  }
  let server = tokio::spawn(async move {
    let mut stream = if numeric {
      numeric_handshake_reply(&listener, true).await
    } else {
      let (mut stream, _) = listener.accept().await.unwrap();
      handshake_reply(&mut stream, None).await;
      stream
    };
    assert!(matches!(
      crate::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
        .await
        .unwrap(),
      Some(LocalControlClientMessage::RestartDaemon)
    ));
    crate::write_local_control_frame(
      &mut stream,
      &LocalControlServerMessage::RestartAccepted {
        terminated_sessions: 2,
      },
    )
    .await
    .unwrap();
    drop(listener);
    std::fs::remove_file(&control).unwrap();
    drop(stream);
    timeout(Duration::from_secs(5), async {
      while !spawned.exists() {
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
    })
    .await
    .unwrap();
    let data = UnixListener::bind(&socket).unwrap();
    let listener = UnixListener::bind(&control).unwrap();
    let (_data_stream, _) = data.accept().await.unwrap();
    let (mut stream, _) = listener.accept().await.unwrap();
    handshake_reply(&mut stream, Some(&info)).await;
  });
  let outcome = fixture
    .client
    .preflight_restart()
    .await
    .unwrap()
    .restart()
    .await;
  server.await.unwrap();
  let args = std::fs::read_to_string(fixture.root.join("spawned")).unwrap();
  assert!(args.contains(fixture.client.socket.to_str().unwrap()));
  outcome
}

#[tokio::test]
async fn legacy_owner_is_replaced_with_the_verified_selected_build() {
  let fixture = Fixture::new().await;
  let result = replacement(&fixture, true, false).await.unwrap();
  assert_eq!(result.after, fixture.info);
  assert_eq!(result.terminated_sessions, 2);
}

#[tokio::test]
async fn wrong_successor_build_is_not_reported_as_success() {
  let fixture = Fixture::new().await;
  let error = replacement(&fixture, false, false).await.unwrap_err();
  assert_eq!(error.code(), "ctmuxd_restart_verification_failed");
  assert!(error.may_have_stopped());
}

#[tokio::test]
async fn closed_pinned_owner_is_non_destructive() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    handshake_reply(&mut stream, None).await;
  });
  let prepared = fixture.client.preflight_restart().await.unwrap();
  server.await.unwrap();
  let error = prepared.restart().await.unwrap_err();
  assert_eq!(error.code(), "ctmuxd_owner_changed");
  assert!(!error.may_have_stopped());
  assert!(!fixture.root.join("spawned").exists());
}

async fn numeric_handshake_reply(listener: &UnixListener, restart_supported: bool) -> Stream {
  let (mut stream, _) = listener.accept().await.unwrap();
  let offer: serde_json::Value = crate::read_local_control_frame(&mut stream)
    .await
    .unwrap()
    .unwrap();
  assert!(offer.get("protocol").is_some());
  drop(stream); // Historical owners close the unknown published envelope.
  let (mut stream, _) = listener.accept().await.unwrap();
  let offer: serde_json::Value = crate::read_local_control_frame(&mut stream)
    .await
    .unwrap()
    .unwrap();
  assert_eq!(
    offer,
    serde_json::json!({"type": "handshake", "protocol_version": 1})
  );
  crate::write_local_control_frame(
    &mut stream,
    &serde_json::json!({
      "type": "handshake_accepted", "protocol_version": 1,
      "data_protocol_version": 13, "build": ctl_core::component::build_info(),
      "restart_supported": restart_supported,
    }),
  )
  .await
  .unwrap();
  stream
}

#[tokio::test]
async fn numeric_owner_observation_and_canceled_preflight_never_restart() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    for _ in 0..2 {
      let mut stream = numeric_handshake_reply(&listener, true).await;
      assert!(
        crate::read_local_control_frame::<_, serde_json::Value>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    }
  });
  let observed = fixture.client.observe().await.unwrap().unwrap();
  assert_eq!(observed.protocols, Vec::<ProtocolInfo>::new());
  assert!(observed.protocol_version.is_none());
  assert!(observed.control_protocol_version.is_none());
  assert!(observed.component_info().is_none());
  assert_eq!(observed.legacy_protocols[1].version, 13);
  let prepared = fixture.client.preflight_restart().await.unwrap();
  drop(prepared);
  server.await.unwrap();
  assert!(!fixture.root.join("spawned").exists());
}

#[tokio::test]
async fn numeric_owner_can_only_be_restarted_on_its_prepared_stream() {
  let fixture = Fixture::new().await;
  let result = replacement(&fixture, true, true).await.unwrap();
  assert_eq!(result.after, fixture.info);
  assert_eq!(result.terminated_sessions, 2);
}

#[tokio::test]
async fn numeric_owner_without_restart_capability_is_preserved() {
  let fixture = Fixture::new().await;
  let listener = fixture.listener();
  let server = tokio::spawn(async move {
    let mut stream = numeric_handshake_reply(&listener, false).await;
    assert!(
      crate::read_local_control_frame::<_, serde_json::Value>(&mut stream)
        .await
        .unwrap()
        .is_none()
    );
  });
  let error = fixture.client.preflight_restart().await.unwrap_err();
  assert_eq!(error.code(), "daemon_restart_unsupported");
  assert!(!error.may_have_stopped());
  server.await.unwrap();
  assert!(!fixture.root.join("spawned").exists());
}

#[tokio::test]
async fn absent_observation_never_starts_the_selected_helper() {
  let fixture = Fixture::new().await;
  assert!(fixture.client.observe().await.unwrap().is_none());
  assert!(!fixture.root.join("spawned").exists());
}
