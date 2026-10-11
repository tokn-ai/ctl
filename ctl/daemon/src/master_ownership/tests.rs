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
  let worker = tokio::spawn(async move {
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

  for stream in listener.incoming() {
    let mut stream = stream.unwrap();
    let directory = directory.clone();
    let path = path.clone();
    std::thread::spawn(move || -> std::io::Result<()> {
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
