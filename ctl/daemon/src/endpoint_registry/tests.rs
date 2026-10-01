use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;
use crate::{ServerMessage, State};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    // Keep synthetic Unix sockets below macOS's short sockaddr_un limit.
    let root = PathBuf::from("/tmp").join(format!("ctld-leases-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    Self(root)
  }

  fn registry(&self) -> Registry {
    Registry::fixture(self.0.join("endpoints"))
  }

  fn state(&self) -> State {
    State {
      endpoint_registry: self.registry(),
      ..State::default()
    }
  }

  fn endpoint(&self) -> MasterEndpoint {
    MasterEndpoint {
      control_path: self.0.join("master.sock"),
      shared: true,
      startup: SharedMasterStartup::Create,
    }
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn target() -> ctl_ipc::SshTarget {
  ctl_ipc::SshTarget {
    destination: "fixture.invalid".into(),
    ssh_config_alias: Some("fixture.invalid".into()),
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  }
}

#[test]
fn missing_record_does_not_create_a_directory() {
  let fixture = Fixture::new();
  assert!(
    fixture
      .registry()
      .load(&crate::control_path(&target()))
      .unwrap()
      .is_none()
  );
  assert!(!fixture.0.join("endpoints").exists());
}

#[test]
fn ownership_scope_and_private_fallback_survive_round_trip() {
  let fixture = Fixture::new();
  let registry = fixture.registry();
  let private = crate::control_path(&target());
  registry.save(&private, &fixture.endpoint()).unwrap();
  let restored = registry.load(&private).unwrap().unwrap();
  assert!(restored.shared);
  assert_eq!(restored.startup, SharedMasterStartup::ExternalOnly);
  assert_eq!(restored.control_path, fixture.endpoint().control_path);
  let mut another_owner = fixture.registry();
  another_owner.namespace = "another-daemon".into();
  assert!(another_owner.load(&private).unwrap().is_none());
  fs::copy(
    registry.path(&private).unwrap().unwrap(),
    another_owner.path(&private).unwrap().unwrap(),
  )
  .unwrap();
  assert!(another_owner.load(&private).is_err());
  let another_target = private.with_file_name("another-target");
  fs::copy(
    registry.path(&private).unwrap().unwrap(),
    registry.path(&another_target).unwrap().unwrap(),
  )
  .unwrap();
  assert!(registry.load(&another_target).is_err());

  let private_endpoint = MasterEndpoint::managed(&target());
  registry.save(&private, &private_endpoint).unwrap();
  let restored = registry.load(&private).unwrap().unwrap();
  assert!(!restored.shared);
  assert_eq!(restored.startup, SharedMasterStartup::PrivateFallback);
  let mut unowned = fixture.endpoint();
  unowned.shared = false;
  assert!(registry.save(&private, &unowned).is_err());
  // A forged on-disk private endpoint must be rejected too.
  let bytes = serde_json::to_vec(&Record {
    version: 1,
    namespace: registry.namespace.clone(),
    private_path: private.clone(),
    control_path: unowned.control_path,
    shared: false,
  })
  .unwrap();
  fs::write(registry.path(&private).unwrap().unwrap(), bytes).unwrap();
  assert!(registry.load(&private).is_err());
}

#[test]
fn rejects_malformed_oversized_public_and_linked_records() {
  let fixture = Fixture::new();
  let registry = fixture.registry();
  let private = crate::control_path(&target());
  registry.save(&private, &fixture.endpoint()).unwrap();
  let path = registry.path(&private).unwrap().unwrap();
  fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
  assert!(registry.load(&private).is_err());
  registry.save(&private, &fixture.endpoint()).unwrap();
  assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
  fs::hard_link(&path, fixture.0.join("linked-record")).unwrap();
  assert!(registry.load(&private).is_err());
  registry.save(&private, &fixture.endpoint()).unwrap();
  fs::write(&path, b"{broken").unwrap();
  assert!(registry.load(&private).is_err());
  fs::write(
    &path,
    vec![b'x'; usize::try_from(MAX_RECORD_BYTES).unwrap() + 1],
  )
  .unwrap();
  assert!(registry.load(&private).is_err());
  fs::remove_file(&path).unwrap();
  std::os::unix::fs::symlink(fixture.0.join("linked-record"), &path).unwrap();
  assert!(registry.load(&private).is_err());
  fs::remove_file(&path).unwrap();
  assert!(
    std::process::Command::new("mkfifo")
      .arg(&path)
      .status()
      .unwrap()
      .success()
  );
  assert!(registry.load(&private).is_err());
}

#[test]
fn unsafe_ancestors_are_rejected_before_creating_descendants() {
  let fixture = Fixture::new();
  let private = crate::control_path(&target());
  fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();
  assert!(
    fixture
      .registry()
      .save(&private, &fixture.endpoint())
      .is_err()
  );
  assert!(!fixture.0.join("endpoints").exists());
  fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
  let linked = fixture.0.join("linked");
  std::os::unix::fs::symlink(&fixture.0, &linked).unwrap();
  let registry = Registry::fixture(linked.join("endpoints"));
  assert!(registry.save(&private, &fixture.endpoint()).is_err());
  assert!(!fixture.0.join("endpoints").exists());
}

#[test]
fn persistence_failure_retains_observed_endpoint_in_memory() {
  let fixture = Fixture::new();
  let state = fixture.state();
  fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();
  let error = state
    .adopt(&target(), &fixture.endpoint(), None)
    .unwrap_err();
  assert_eq!(error.code(), "ssh_status_unknown");
  assert_eq!(
    state
      .existing_endpoint(&target())
      .unwrap()
      .unwrap()
      .control_path,
    fixture.endpoint().control_path
  );
}

async fn mux_alive(listener: tokio::net::UnixListener) {
  let (mut stream, _) = listener.accept().await.unwrap();
  assert_eq!(stream.read_u32().await.unwrap(), 8);
  assert_eq!(stream.read_u32().await.unwrap(), 1); // MUX_MSG_HELLO
  assert_eq!(stream.read_u32().await.unwrap(), 4); // protocol version
  for value in [8, 1, 4] {
    stream.write_u32(value).await.unwrap();
  }
  assert_eq!(stream.read_u32().await.unwrap(), 8);
  assert_eq!(stream.read_u32().await.unwrap(), 0x1000_0004); // MUX_C_ALIVE_CHECK
  let request = stream.read_u32().await.unwrap();
  for value in [12, 0x8000_0005, request, std::process::id()] {
    stream.write_u32(value).await.unwrap();
  }
}

#[tokio::test]
async fn restarted_state_observes_saved_mux_without_adopting_or_config_evaluation() {
  let fixture = Fixture::new();
  fixture
    .state()
    .adopt(&target(), &fixture.endpoint(), None)
    .unwrap();
  let restarted = fixture.state();
  let listener = tokio::net::UnixListener::bind(fixture.endpoint().control_path).unwrap();
  let master = tokio::spawn(mux_alive(listener));
  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  crate::connection_status(&mut server, &restarted, &target())
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::ConnectionStatus {
      connected: true,
      manually_disconnected: false
    })
  ));
  master.await.unwrap();
  assert_eq!(
    restarted
      .configured_connections
      .lock()
      .unwrap()
      .keys()
      .collect::<Vec<_>>(),
    Vec::<&String>::new()
  );

  // A disappeared endpoint proves absence; discovery must never start SSH.
  fs::remove_file(fixture.endpoint().control_path).unwrap();
  crate::connection_status(&mut server, &restarted, &target())
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::ConnectionStatus {
      connected: false,
      manually_disconnected: false
    })
  ));
}

#[tokio::test]
async fn master_lookup_and_shared_disconnect_use_the_saved_endpoint() {
  let fixture = Fixture::new();
  fixture
    .state()
    .adopt(&target(), &fixture.endpoint(), None)
    .unwrap();
  let restarted = fixture.state();
  let listener = tokio::net::UnixListener::bind(fixture.endpoint().control_path).unwrap();
  let master = tokio::spawn(mux_alive(listener));
  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  crate::master_status(&mut server, &restarted, &target())
    .await
    .unwrap();
  assert!(
    matches!(ctl_ipc::read_frame::<_, ServerMessage>(&mut client).await.unwrap(),
    Some(ServerMessage::MasterReady {control_path}) if control_path == fixture.endpoint().control_path)
  );
  master.await.unwrap();
  crate::disconnect_master(&mut server, &restarted, &target())
    .await
    .unwrap();
  assert!(fixture.endpoint().control_path.exists());
  assert!(restarted.target(&target()).is_paused());
  assert!(
    fixture
      .state()
      .existing_endpoint(&target())
      .unwrap()
      .is_none()
  );
}

#[tokio::test]
async fn paused_status_does_not_read_even_malformed_persisted_metadata() {
  let fixture = Fixture::new();
  let state = Arc::new(fixture.state());
  state.adopt(&target(), &fixture.endpoint(), None).unwrap();
  fs::write(
    fixture
      .registry()
      .path(&crate::control_path(&target()))
      .unwrap()
      .unwrap(),
    b"bad record",
  )
  .unwrap();
  let restarted = fixture.state();
  restarted.target(&target()).pause();
  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  crate::connection_status(&mut server, &restarted, &target())
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::ConnectionStatus {
      connected: false,
      manually_disconnected: true
    })
  ));
}
