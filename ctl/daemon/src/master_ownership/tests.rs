use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};

use super::{CleanupBarrier, PendingMaster};
use crate::{MasterEndpoint, SharedMasterStartup};

struct PendingRequest {
  state: Arc<crate::State>,
  client: ctl_ipc::Stream,
  worker: tokio::task::JoinHandle<Result<(), crate::RequestError>>,
  finalized: oneshot::Sender<()>,
  recovery: tokio::sync::watch::Receiver<Option<u64>>,
  _lease: crate::wake_recovery::Lease,
}

fn request(
  owner: PendingMaster,
  endpoint: MasterEndpoint,
  barrier: CleanupBarrier,
) -> PendingRequest {
  let (client, mut server) = ctl_ipc::Stream::pair().unwrap();
  let (finalized, finalization) = oneshot::channel();
  let state = Arc::new(crate::State::default());
  let target = ctl_ipc::SshTarget {
    destination: "fixture.invalid".into(),
    ssh_config_alias: None,
    use_ssh_config_master: Some(false),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  };
  let (lease, recovery) = state.wake_recovery.register(target.clone(), 0).unwrap();
  let capture = crate::manual_recovery::Capture::new(&state, &target);
  let worker_state = Arc::clone(&state);
  let worker = tokio::spawn(async move {
    let state = worker_state;
    let monitor = crate::client_lifetime::Monitor::new(&server).unwrap();
    let work = async move {
      // This barrier models asynchronous authorization finalization, after
      // socket publication and before the production Ready/commit boundary.
      finalization
        .await
        .map_err(|_| crate::RequestError::ClientClosed)?;
      crate::publish_master_ready(
        &mut server,
        &state,
        &target,
        &endpoint,
        owner,
        None,
        Some(capture.prepare(&state).await),
      )
      .await
    };
    let result = tokio::select! {
      biased;
      _ = monitor.closed() => Err(crate::RequestError::ClientClosed),
      result = work => result,
    };
    barrier.wait().await.unwrap();
    result
  });
  PendingRequest {
    state,
    client,
    worker,
    finalized,
    recovery,
    _lease: lease,
  }
}

struct Fixture {
  directory: PathBuf,
  endpoint: MasterEndpoint,
  child: Option<Child>,
  master: std::process::Child,
  _process_guard: ctl_core::test_fixtures::ProcessGuard,
}

impl Fixture {
  async fn start() -> Self {
    let process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let directory = std::env::temp_dir().join(format!("ctld-owner-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let endpoint = MasterEndpoint {
      control_path: directory.join("master"),
      shared: false,
      startup: SharedMasterStartup::PrivateFallback,
    };
    let master = std::process::Command::new(std::env::current_exe().unwrap())
      .args([
        "--exact",
        "master_ownership::tests::mux_fixture_process",
        "--ignored",
      ])
      .env("CTLD_MASTER_FIXTURE_DIRECTORY", &directory)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::inherit())
      .spawn()
      .unwrap();
    let mut fixture = Self {
      directory,
      endpoint,
      child: None,
      master,
      _process_guard: process_guard,
    };
    wait_for_path(&fixture.directory.join("ready")).await;
    // Model the post-fork state: ssh's tracked foreground process has exited,
    // while a distinct master process continues owning the control socket.
    // Keep the fixture process as our direct child so every test reaps it.
    let mut child = Command::new("/bin/sh")
      .args(["-c", "exit 0"])
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    assert!(child.wait().await.unwrap().success());
    fixture.child = Some(child);
    fixture
  }

  fn owner(&mut self, lock: &Arc<Mutex<()>>, barrier: &CleanupBarrier) -> PendingMaster {
    PendingMaster::new(
      self.child.take().unwrap(),
      &self.endpoint,
      Arc::clone(lock).try_lock_owned().unwrap(),
      barrier,
    )
  }

  fn release(&self) {
    std::fs::write(self.directory.join("release"), "").unwrap();
  }

  async fn terminated(&self) {
    wait_for_path(&self.directory.join("terminated")).await;
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = self.master.kill();
    let _ = self.master.wait();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

#[test]
#[ignore = "isolated local mux fixture subprocess"]
fn mux_fixture_process() {
  use std::io::{Read as _, Write as _};
  use std::os::unix::fs::MetadataExt as _;
  use std::os::unix::net::{UnixListener, UnixStream};

  fn packet(stream: &mut UnixStream, values: &[u32]) -> std::io::Result<()> {
    let payload: Vec<_> = values.iter().flat_map(|word| word.to_be_bytes()).collect();
    stream.write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())?;
    stream.write_all(&payload)
  }

  let Some(directory) = std::env::var_os("CTLD_MASTER_FIXTURE_DIRECTORY") else {
    return;
  };
  let directory = PathBuf::from(directory);
  let path = directory.join("master");
  let listener = UnixListener::bind(&path).unwrap();
  let identity = std::fs::metadata(&path).unwrap().ino();
  std::fs::write(directory.join("ready"), "").unwrap();

  for (id, stream) in listener.incoming().enumerate() {
    let mut stream = stream.unwrap();
    std::fs::write(directory.join(format!("accepted-{id}")), "").unwrap();
    let directory = directory.clone();
    let path = path.clone();
    std::thread::spawn(move || -> std::io::Result<()> {
      struct Closed(PathBuf);
      impl Drop for Closed {
        fn drop(&mut self) {
          let _ = std::fs::write(&self.0, "");
        }
      }
      let _closed = Closed(directory.join(format!("closed-{id}")));
      packet(&mut stream, &[1, 4])?;
      loop {
        let mut length = [0; 4];
        if stream.read_exact(&mut length).is_err() {
          return Ok(());
        }
        let length = u32::from_be_bytes(length) as usize;
        assert_eq!(length, 8);
        let mut payload = [0; 8];
        stream.read_exact(&mut payload)?;
        let kind = u32::from_be_bytes(payload[..4].try_into().unwrap());
        let value = u32::from_be_bytes(payload[4..].try_into().unwrap());
        match kind {
          1 => assert_eq!(value, 4),
          0x1000_0004 => packet(&mut stream, &[0x8000_0005, value, std::process::id()])?,
          0x1000_0005 => {
            std::fs::write(directory.join("terminating"), "")?;
            while !directory.join("release").exists() {
              std::thread::sleep(Duration::from_millis(5));
            }
            if std::fs::metadata(&path).is_ok_and(|metadata| metadata.ino() == identity) {
              std::fs::remove_file(&path)?;
            }
            if !directory.join("omit-ack").exists() {
              packet(&mut stream, &[0x8000_0001, value])?;
            }
            std::fs::write(directory.join("terminated"), "")?;
            std::process::exit(0);
          }
          other => panic!("unsupported fixture mux packet {other}"),
        }
      }
    });
  }
}

async fn wait_for_path(path: &Path) {
  tokio::time::timeout(Duration::from_secs(5), async {
    while !path.exists() {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .unwrap();
}

#[tokio::test]
async fn canceled_postfork_master_finishes_cleanup_before_releasing_target_lock() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  drop(owner);
  wait_for_path(&fixture.directory.join("terminating")).await;
  assert!(lock.try_lock().is_err());
  let waiting = tokio::spawn(async move { barrier.wait().await });
  assert!(!waiting.is_finished());
  fixture.release();
  waiting.await.unwrap().unwrap();
  fixture.terminated().await;
  assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn eof_during_authorization_finalization_waits_for_private_cleanup() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  let request = request(owner, fixture.endpoint.clone(), barrier);
  drop(request.client);
  wait_for_path(&fixture.directory.join("terminating")).await;
  assert!(lock.try_lock().is_err());
  assert!(!request.worker.is_finished());
  fixture.release();
  assert!(matches!(
    request.worker.await.unwrap(),
    Err(crate::RequestError::ClientClosed)
  ));
  assert_eq!(*request.recovery.borrow(), None);
  fixture.terminated().await;
  assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn ready_waits_for_finalization_and_immediate_client_eof_keeps_the_master() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  let mut request = request(owner, fixture.endpoint.clone(), barrier);
  wait_for_path(&fixture.directory.join("accepted-0")).await;
  assert!(request.state.private_masters.lock().unwrap().is_empty());
  assert!(
    tokio::time::timeout(
      Duration::from_millis(30),
      ctl_ipc::read_frame::<_, crate::ServerMessage>(&mut request.client),
    )
    .await
    .is_err()
  );
  assert_eq!(*request.recovery.borrow(), None);
  request.finalized.send(()).unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, crate::ServerMessage>(&mut request.client)
      .await
      .unwrap(),
    Some(crate::ServerMessage::MasterReady { .. })
  ));
  assert_eq!(*request.recovery.borrow(), Some(1));
  drop(request.client);
  let _ = request.worker.await.unwrap();
  assert_eq!(request.state.private_masters.lock().unwrap().len(), 1);
  assert!(
    tokio::time::timeout(
      Duration::from_millis(30),
      wait_for_path(&fixture.directory.join("closed-0")),
    )
    .await
    .is_err()
  );
  drop(request.state);
  wait_for_path(&fixture.directory.join("closed-0")).await;
  assert!(!fixture.directory.join("terminating").exists());
  assert!(fixture.endpoint.control_path.exists());
  assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn cancellation_before_pin_still_terminates_the_postfork_master() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  fixture.release();
  drop(fixture.owner(&lock, &barrier));
  barrier.wait().await.unwrap();
  fixture.terminated().await;
  assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn a_master_exiting_before_its_termination_ack_is_successful_cleanup() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  std::fs::write(fixture.directory.join("omit-ack"), "").unwrap();
  fixture.release();
  drop(owner);
  barrier.wait().await.unwrap();
  fixture.terminated().await;
  assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn dropping_an_owner_without_a_runtime_uses_the_pinned_cleanup_connection() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  fixture.release();
  std::thread::spawn(move || drop(owner)).join().unwrap();
  barrier.wait().await.unwrap();
  fixture.terminated().await;
  assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn a_stalled_master_reports_cleanup_failure_and_releases_the_target_lock() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  let started = std::time::Instant::now();
  drop(owner);
  assert!(barrier.wait().await.is_err());
  assert!(started.elapsed() < Duration::from_secs(3));
  assert!(lock.try_lock().is_ok());
  assert!(fixture.directory.join("terminating").exists());
}

#[tokio::test]
async fn a_pinned_canceled_master_does_not_terminate_its_socket_replacement() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  std::fs::remove_file(&fixture.endpoint.control_path).unwrap();
  let replacement = tokio::net::UnixListener::bind(&fixture.endpoint.control_path).unwrap();
  fixture.release();
  drop(owner);
  barrier.wait().await.unwrap();
  fixture.terminated().await;
  assert!(fixture.endpoint.control_path.exists());
  assert!(
    tokio::time::timeout(Duration::from_millis(30), replacement.accept())
      .await
      .is_err()
  );
}

#[tokio::test]
async fn committing_an_owned_private_master_keeps_it_alive() {
  let mut fixture = Fixture::start().await;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  drop(owner.connected());
  barrier.wait().await.unwrap();
  assert!(lock.try_lock().is_ok());
  // Use the real OpenSSH mux client to check the synthetic master remains
  // reachable and recognizes the documented v4 framing.
  let status = Command::new("ssh")
    .args(["-F", "none", "-S"])
    .arg(&fixture.endpoint.control_path)
    .args(["-O", "check", "fixture.invalid"])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .status()
    .await
    .unwrap();
  assert!(status.success());
  assert!(!fixture.directory.join("terminating").exists());
}

fn configured_target() -> ctl_ipc::SshTarget {
  ctl_ipc::SshTarget {
    destination: "fixture.invalid".into(),
    ssh_config_alias: Some("fixture.invalid".into()),
    use_ssh_config_master: Some(true),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  }
}

#[tokio::test]
async fn repeated_private_reuse_retains_one_client_until_disconnect() {
  let fixture = Fixture::start().await;
  let state = crate::State::default();
  let target = configured_target();
  state.adopt(&target, &fixture.endpoint, None).unwrap();
  let lease = state
    .private_lease_for_reuse(&target, &fixture.endpoint)
    .await
    .unwrap();
  state.retain_private_master(&target, lease);
  wait_for_path(&fixture.directory.join("accepted-0")).await;
  for _ in 0..32 {
    let lease = state
      .private_lease_for_reuse(&target, &fixture.endpoint)
      .await
      .unwrap();
    assert!(lease.is_none());
    state.retain_private_master(&target, lease);
    state.adopt(&target, &fixture.endpoint, None).unwrap();
  }
  assert!(!fixture.directory.join("accepted-1").exists());
  assert!(!fixture.directory.join("closed-0").exists());
  fixture.release();
  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  crate::disconnect_master(&mut server, &state, &target)
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, crate::ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(crate::ServerMessage::MasterDisconnected)
  ));
  assert!(state.private_masters.lock().unwrap().is_empty());
  fixture.terminated().await;
}

#[tokio::test]
async fn a_socket_replacement_gets_its_own_lease_without_terminating_either_master() {
  let fixture = Fixture::start().await;
  let state = crate::State::default();
  let target = configured_target();
  let lease = state
    .private_lease_for_reuse(&target, &fixture.endpoint)
    .await
    .unwrap();
  state.retain_private_master(&target, lease);
  wait_for_path(&fixture.directory.join("accepted-0")).await;
  std::fs::remove_file(&fixture.endpoint.control_path).unwrap();
  let replacement = tokio::net::UnixListener::bind(&fixture.endpoint.control_path).unwrap();
  let lease = state
    .private_lease_for_reuse(&target, &fixture.endpoint)
    .await
    .unwrap();
  assert!(lease.is_some());
  let (mut replacement_client, _) = replacement.accept().await.unwrap();
  state.retain_private_master(&target, lease);
  wait_for_path(&fixture.directory.join("closed-0")).await;
  assert!(!fixture.directory.join("terminating").exists());
  assert!(fixture.endpoint.control_path.exists());
  assert_eq!(state.private_masters.lock().unwrap().len(), 1);
  drop(state);
  let mut byte = [0];
  assert_eq!(replacement_client.read(&mut byte).await.unwrap(), 0);
}

#[tokio::test]
async fn orderly_shutdown_releases_private_clients_and_shared_anchors_while_state_is_shared() {
  let fixture = Fixture::start().await;
  let state = Arc::new(crate::State::default());
  let background_reference = Arc::clone(&state);
  for destination in ["first.invalid", "second.invalid"] {
    let mut target = configured_target();
    target.destination = destination.into();
    let lease = state
      .private_lease_for_reuse(&target, &fixture.endpoint)
      .await
      .unwrap();
    state.retain_private_master(&target, lease);
  }
  for id in 0..2 {
    wait_for_path(&fixture.directory.join(format!("accepted-{id}"))).await;
    assert!(!fixture.directory.join(format!("closed-{id}")).exists());
  }
  assert_eq!(state.private_masters.lock().unwrap().len(), 2);
  // A shared master's anchor is an owned session-input pipe. Model only that
  // ownership boundary with cat: EOF must arrive even if State survives.
  let mut anchor = Command::new("/bin/cat")
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let mut shared_endpoint = fixture.endpoint.clone();
  shared_endpoint.shared = true;
  state.remember_endpoint(&configured_target(), &shared_endpoint, anchor.stdin.take());
  assert!(anchor.try_wait().unwrap().is_none());
  state.release_master_leases();
  drop(state);
  for id in 0..2 {
    wait_for_path(&fixture.directory.join(format!("closed-{id}"))).await;
  }
  assert!(
    background_reference
      .private_masters
      .lock()
      .unwrap()
      .is_empty()
  );
  assert!(
    background_reference
      .configured_connections
      .lock()
      .unwrap()
      .is_empty()
  );
  assert!(
    tokio::time::timeout(Duration::from_secs(5), anchor.wait())
      .await
      .unwrap()
      .unwrap()
      .success()
  );
  assert!(fixture.endpoint.control_path.exists());
  assert!(!fixture.directory.join("terminating").exists());
}

#[tokio::test]
async fn passive_status_does_not_retain_private_clients_or_change_shared_ownership() {
  let fixture = Fixture::start().await;
  let state = crate::State::default();
  let target = configured_target();
  state.adopt(&target, &fixture.endpoint, None).unwrap();
  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  crate::connection_status(&mut server, &state, &target)
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, crate::ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(crate::ServerMessage::ConnectionStatus {
      connected: true,
      ..
    })
  ));
  wait_for_path(&fixture.directory.join("closed-0")).await;
  assert!(state.private_masters.lock().unwrap().is_empty());
  let mut shared = fixture.endpoint.clone();
  shared.shared = true;
  assert!(
    state
      .private_lease_for_reuse(&target, &shared)
      .await
      .unwrap()
      .is_none()
  );
  assert!(!fixture.directory.join("accepted-1").exists());
}

#[test]
#[ignore = "isolated native lease owner subprocess"]
fn native_lease_owner_process() {
  let Some(path) = std::env::var_os("CTLD_LEASE_FIXTURE_ENDPOINT") else {
    return;
  };
  let ready = std::env::var_os("CTLD_LEASE_FIXTURE_READY").unwrap();
  let endpoint = MasterEndpoint {
    control_path: PathBuf::from(path),
    shared: false,
    startup: SharedMasterStartup::PrivateFallback,
  };
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  let _lease = runtime
    .block_on(super::PrivateMasterLease::attach(&endpoint))
    .unwrap();
  std::fs::write(ready, "").unwrap();
  loop {
    std::thread::park();
  }
}

#[tokio::test]
async fn killing_the_lease_owner_closes_its_client_without_terminating_the_master() {
  let fixture = Fixture::start().await;
  let ready = fixture.directory.join("owner-ready");
  let mut owner = Command::new(std::env::current_exe().unwrap())
    .args([
      "--exact",
      "master_ownership::tests::native_lease_owner_process",
      "--ignored",
    ])
    .env(
      "CTLD_LEASE_FIXTURE_ENDPOINT",
      &fixture.endpoint.control_path,
    )
    .env("CTLD_LEASE_FIXTURE_READY", &ready)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::inherit())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  wait_for_path(&ready).await;
  wait_for_path(&fixture.directory.join("accepted-0")).await;
  owner.kill().await.unwrap();
  wait_for_path(&fixture.directory.join("closed-0")).await;
  assert!(fixture.endpoint.control_path.exists());
  assert!(!fixture.directory.join("terminating").exists());
}

struct RealSshServer {
  directory: PathBuf,
  child: std::process::Child,
  control: super::private_control::Control,
  endpoint: MasterEndpoint,
  user: String,
  port: u16,
}

impl Drop for RealSshServer {
  fn drop(&mut self) {
    let _ = self.control.terminate();
    let _ = self.child.kill();
    let _ = self.child.wait();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

impl RealSshServer {
  async fn start() -> Self {
    use std::os::unix::fs::DirBuilderExt as _;
    // OpenSSH appends its own publication suffix, so Darwin's ordinary
    // TMPDIR can exceed the Unix-socket pathname limit even for a short name.
    let directory = PathBuf::from("/tmp").join(format!("ctld-real-mux-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&directory)
      .unwrap();
    for name in ["host", "identity"] {
      assert!(
        Command::new("ssh-keygen")
          .args(["-q", "-t", "ed25519", "-N", "", "-f"])
          .arg(directory.join(name))
          .kill_on_drop(true)
          .status()
          .await
          .unwrap()
          .success()
      );
    }
    std::fs::copy(
      directory.join("identity.pub"),
      directory.join("authorized_keys"),
    )
    .unwrap();
    let user = Command::new("id").arg("-un").output().await.unwrap();
    assert!(user.status.success());
    let user = String::from_utf8(user.stdout).unwrap().trim().to_owned();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
      .unwrap()
      .local_addr()
      .unwrap()
      .port();
    let endpoint = MasterEndpoint {
      control_path: directory.join("mux"),
      shared: false,
      startup: SharedMasterStartup::PrivateFallback,
    };
    let configuration = directory.join("sshd_config");
    std::fs::write(&configuration, format!(
      "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nStrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\nAllowUsers {user}\nLogLevel ERROR\n",
      directory.join("host").display(),
      directory.join("sshd.pid").display(),
      directory.join("authorized_keys").display(),
    )).unwrap();
    let child = std::process::Command::new("/usr/sbin/sshd")
      .args(["-D", "-e", "-f"])
      .arg(configuration)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::from(
        std::fs::File::create(directory.join("sshd.log")).unwrap(),
      ))
      .spawn()
      .unwrap();
    let mut server = Self {
      control: super::private_control::Control::new(&endpoint),
      directory,
      child,
      endpoint,
      user,
      port,
    };
    tokio::time::timeout(Duration::from_secs(5), async {
      loop {
        assert!(
          server.child.try_wait().unwrap().is_none(),
          "isolated sshd exited"
        );
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
          .await
          .is_ok()
        {
          break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
      }
    })
    .await
    .unwrap();
    server
  }

  fn master_command(&self) -> Command {
    let mut command = Command::new("ssh");
    command
      .args(["-F", "none", "-f", "-M", "-N", "-T", "-S"])
      .arg(&self.endpoint.control_path)
      .arg("-p")
      .arg(self.port.to_string())
      .arg("-i")
      .arg(self.directory.join("identity"))
      .args([
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "IdentityAgent=none",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "ControlPersist=1",
        "-o",
        "ServerAliveInterval=1",
        "-o",
        "ServerAliveCountMax=3",
      ])
      .arg("-o")
      .arg(format!(
        "UserKnownHostsFile={}",
        self.directory.join("known_hosts").display()
      ))
      .arg(format!("{}@127.0.0.1", self.user))
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      // A daemonized master may inherit stderr. A pipe would make output()
      // wait for the master itself to expire, rather than just its -f parent.
      .stderr(Stdio::from(
        std::fs::File::create(self.directory.join("ssh.log")).unwrap(),
      ))
      .kill_on_drop(true);
    command
  }
}

/// Uses only a transient loopback server and freshly generated keys. CI's
/// protocol fixture tests do not require sshd installation.
#[tokio::test]
#[ignore = "requires /usr/sbin/sshd and loopback access; uses synthetic keys only"]
async fn a_native_lease_suspends_real_openssh_idle_expiry_until_released() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let server = RealSshServer::start().await;
  let status = tokio::time::timeout(Duration::from_secs(5), server.master_command().status())
    .await
    .unwrap()
    .unwrap();
  assert!(
    status.success(),
    "synthetic SSH failed: {}",
    std::fs::read_to_string(server.directory.join("ssh.log")).unwrap()
  );
  let lease = super::PrivateMasterLease::attach(&server.endpoint)
    .await
    .unwrap();
  // This is intentionally longer than the configured real OpenSSH timer.
  tokio::time::sleep(Duration::from_secs(3)).await;
  assert!(
    crate::control_master_is_ready(&configured_target(), &server.endpoint.control_path)
      .await
      .unwrap()
  );
  drop(lease);
  tokio::time::timeout(Duration::from_secs(4), async {
    while server.endpoint.control_path.exists() {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .unwrap();
}

#[tokio::test]
async fn shared_master_cancellation_only_releases_our_process() {
  let mut fixture = Fixture::start().await;
  fixture.endpoint.shared = true;
  let lock = Arc::new(Mutex::new(()));
  let barrier = CleanupBarrier::default();
  let mut owner = fixture.owner(&lock, &barrier);
  owner.pin().await.unwrap();
  drop(owner);
  barrier.wait().await.unwrap();
  assert!(lock.try_lock().is_ok());
  assert!(!fixture.directory.join("terminating").exists());
}

#[tokio::test]
async fn malformed_mux_packets_are_bounded_and_do_not_send_terminate() {
  let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
  let worker = tokio::task::spawn_blocking(move || {
    let mut client = client.into_std().unwrap();
    super::private_control::terminate(&mut client)
  });
  let mut hello = [0_u8; 12];
  server.read_exact(&mut hello).await.unwrap();
  server.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
  assert_eq!(
    worker.await.unwrap().unwrap_err().kind(),
    std::io::ErrorKind::InvalidData
  );
  assert_eq!(server.read(&mut hello).await.unwrap(), 0);
}

fn closed_termination_peer(acknowledge: bool) -> std::os::unix::net::UnixStream {
  use std::io::{Read as _, Write as _};
  use std::os::unix::net::UnixStream;

  let (mut client, mut server) = UnixStream::pair().unwrap();
  let request = [8_u32, 0x1000_0005, 0]
    .into_iter()
    .flat_map(u32::to_be_bytes)
    .collect::<Vec<_>>();
  client.write_all(&request).unwrap();
  let mut received = [0; 12];
  server.read_exact(&mut received).unwrap();
  assert_eq!(received.as_slice(), request.as_slice());
  if acknowledge {
    let response = [8_u32, 0x8000_0001, 0]
      .into_iter()
      .flat_map(u32::to_be_bytes)
      .collect::<Vec<_>>();
    server.write_all(&response).unwrap();
  }
  client.set_nonblocking(true).unwrap();
  // Peer shutdown happens before any response read. On Darwin, this also
  // prohibits setsockopt, even while an OK packet remains buffered to read.
  server.shutdown(std::net::Shutdown::Both).unwrap();
  drop(server);
  #[cfg(target_os = "macos")]
  assert_eq!(
    client
      .set_read_timeout(Some(Duration::from_secs(1)))
      .unwrap_err()
      .raw_os_error(),
    Some(22)
  );
  client
}

#[test]
fn a_closed_peer_with_a_buffered_termination_ack_remains_readable() {
  let mut client = closed_termination_peer(true);
  super::private_control::read_termination_response(
    &mut client,
    std::time::Instant::now() + crate::MASTER_CHECK_TIMEOUT,
  )
  .unwrap();
}

#[test]
fn a_closed_peer_without_a_termination_ack_is_expected_eof() {
  let mut client = closed_termination_peer(false);
  super::private_control::read_termination_response(
    &mut client,
    std::time::Instant::now() + crate::MASTER_CHECK_TIMEOUT,
  )
  .unwrap();
}

#[tokio::test]
async fn a_trickling_mux_peer_cannot_extend_the_cleanup_deadline() {
  let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
  let started = std::time::Instant::now();
  let worker = tokio::task::spawn_blocking(move || {
    super::private_control::terminate(&mut client.into_std().unwrap())
  });
  let mut hello = [0_u8; 12];
  server.read_exact(&mut hello).await.unwrap();
  server.write_all(&8_u32.to_be_bytes()).await.unwrap();
  let trickle = tokio::spawn(async move {
    for byte in [0_u8; 8] {
      tokio::time::sleep(Duration::from_millis(400)).await;
      if server.write_all(&[byte]).await.is_err() {
        break;
      }
    }
  });
  assert!(worker.await.unwrap().is_err());
  assert!(started.elapsed() < Duration::from_secs(3));
  trickle.abort();
  let _ = trickle.await;
}
