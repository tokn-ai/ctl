use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Command};
use std::time::Instant;

use super::*;

const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Fixture {
  root: PathBuf,
  engine: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctld-shared-vpn-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let engine = root.join("engine");
    fs::write(
      &engine,
      r#"#!/bin/sh
set -eu
root=${0%/*}
printf '%s\n' "$*" >> "$root/calls"
case "$1" in
  ps) cat "$root/inventory" ;;
  container)
    id=$3
    if [ -f "$root/unavailable" ]; then
      printf '%s\n' 'Cannot connect to the container engine' >&2
      exit 1
    fi
    if [ -f "$root/delay-inspect" ]; then
      mkdir "$root/active-$$"
      set -- "$root"/active-*
      printf '%s\n' "$#" >> "$root/concurrency"
      sleep 0.1
      rmdir "$root/active-$$"
    fi
    if [ -f "$root/inspect-$id" ]; then
      cat "$root/inspect-$id"
    else
      printf '%s\n' "Error: No such container: $id" >&2
      exit 1
    fi
    ;;
  exec) [ ! -f "$root/heartbeat-failed" ] ;;
  rm) : ;;
  *) exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("inventory"), "").unwrap();
    Self { root, engine }
  }

  fn inspect(&self, name: &str, value: &serde_json::Value) {
    fs::write(
      self.root.join(format!("inspect-{name}")),
      serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
  }

  fn calls(&self) -> Vec<String> {
    fs::read_to_string(self.root.join("calls"))
      .unwrap_or_default()
      .lines()
      .map(str::to_owned)
      .collect()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.root);
  }
}

fn connection() -> VpnConnection {
  VpnConnection {
    connection_id: "test-profile".into(),
    name: "Test profile".into(),
    settings: VpnSettings::Openconnect {
      url: "https://gateway.invalid/groups/private?token=test-query-token".into(),
      username: "test-user".into(),
      password: "test-secret-password".to_owned().into(),
      auth_method: Some("password".into()),
      target_ip: None,
    },
  }
}

fn inspection(
  metadata: &RuntimeMetadata,
  id: &str,
  running: bool,
  state: &str,
) -> serde_json::Value {
  let mut labels: HashMap<String, String> = labels_arguments(metadata)
    .unwrap()
    .as_chunks::<2>()
    .0
    .iter()
    .map(|argument| argument[1].split_once('=').unwrap())
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
  labels.insert("io.ctl.vpn.creator".into(), "creator-test".into());
  serde_json::json!([{
    "Id": id,
    "Name": format!("/{}", container_name(metadata).unwrap()),
    "Config": { "Labels": labels },
    "State": { "Running": running, "ExitCode": 0, "Status": state, "Health": { "Status": "healthy" } },
    "NetworkSettings": { "Ports": { "1080/tcp": [{ "HostIp": "127.0.0.1", "HostPort": "23456" }] } }
  }])
}

#[tokio::test]
async fn shared_inventory_inspections_are_parallel_with_a_fixed_concurrency_limit() {
  let fixture = Fixture::new();
  fs::write(fixture.root.join("delay-inspect"), "").unwrap();
  let mut inventory = String::new();
  for index in 0..(INSPECT_CONCURRENCY * 2) {
    let id = format!("{index:064x}");
    let mut profile = connection();
    profile.connection_id = format!("profile-{index}");
    let metadata = RuntimeMetadata::for_connection(&profile).unwrap();
    writeln!(inventory, "{id}").unwrap();
    fixture.inspect(&id, &inspection(&metadata, &id, true, "running"));
  }
  fs::write(fixture.root.join("inventory"), inventory).unwrap();
  let observed = list(&fixture.engine).await.unwrap();
  assert_eq!(observed.len(), INSPECT_CONCURRENCY * 2);
  let parallelism = fs::read_to_string(fixture.root.join("concurrency"))
    .unwrap()
    .lines()
    .map(|line| line.parse::<usize>().unwrap())
    .max()
    .unwrap();
  assert!(parallelism > 1);
  assert!(parallelism <= INSPECT_CONCURRENCY);
  assert!(
    fixture
      .calls()
      .iter()
      .all(|call| call.starts_with("ps ") || call.starts_with("container inspect "))
  );
}

#[test]
fn created_reservations_are_starting_and_keep_profiles_reserved() {
  let metadata = RuntimeMetadata::for_connection(&connection()).unwrap();
  let descriptor = parse_descriptor(
    Path::new("engine"),
    ID,
    &serde_json::to_vec(&inspection(&metadata, ID, false, "created")).unwrap(),
  )
  .unwrap();
  let status = descriptor.basic_status();
  assert_eq!(status.state, VpnState::Starting);
  assert!(!status.running);
  assert_eq!(status.endpoint, None);
  assert_eq!(status.locally_connected, Some(false));
}

#[test]
fn routing_settings_detect_endpoint_changes_without_exposing_credentials() {
  let connection = connection();
  let metadata = RuntimeMetadata::for_connection(&connection).unwrap();
  let serialized = labels_arguments(&metadata).unwrap().join(" ");
  assert_eq!(metadata.vpn_url.as_deref(), Some("https://gateway.invalid"));
  for private in [
    "test-secret-password",
    "test-query-token",
    "/groups/private",
  ] {
    assert!(!serialized.contains(private));
  }
  let mut changed = connection.clone();
  let VpnSettings::Openconnect { password, .. } = &mut changed.settings else {
    unreachable!()
  };
  *password = "another-secret".to_owned().into();
  assert_eq!(metadata, RuntimeMetadata::for_connection(&changed).unwrap());
  for endpoint in [
    "https://gateway.invalid/other?token=test-query-token",
    "https://gateway.invalid/groups/private?token=other-token",
  ] {
    let VpnSettings::Openconnect { url, .. } = &mut changed.settings else {
      unreachable!()
    };
    *url = endpoint.into();
    let changed = RuntimeMetadata::for_connection(&changed).unwrap();
    assert_ne!(metadata.settings_key, changed.settings_key);
    let descriptor = parse_descriptor(
      Path::new("engine"),
      ID,
      &serde_json::to_vec(&inspection(&metadata, ID, true, "running")).unwrap(),
    )
    .unwrap();
    assert!(descriptor.compatible(&changed).is_err());
  }
}

#[test]
fn environment_fingerprint_ignores_password_but_checks_the_complete_routing_url() {
  let base = "VPN_URL=https://gateway.invalid/group?token=secret-token\nVPN_USERNAME=test-user\nVPN_PASSWORD=private-password\nVPN_AUTH_METHOD=password\n";
  let metadata = RuntimeMetadata::for_env(
    Path::new("profile.env"),
    base,
    Some("https://gateway.invalid/group?token=secret-token".into()),
    Some("test-user".into()),
  )
  .unwrap();
  let changed = RuntimeMetadata::for_env(
    Path::new("profile.env"),
    &base.replace("private-password", "new-password"),
    metadata.vpn_url.clone(),
    metadata.username.clone(),
  )
  .unwrap();
  assert_eq!(metadata, changed);
  let descriptor = parse_descriptor(
    Path::new("engine"),
    ID,
    &serde_json::to_vec(&inspection(&metadata, ID, true, "running")).unwrap(),
  )
  .unwrap();
  assert_eq!(descriptor.basic_status().connection_id, None);
  let changed = RuntimeMetadata::for_env(
    Path::new("profile.env"),
    &base.replace("/group?", "/different?"),
    metadata.vpn_url.clone(),
    metadata.username.clone(),
  )
  .unwrap();
  assert_ne!(metadata.settings_key, changed.settings_key);
  let labels = labels_arguments(&metadata).unwrap().join(" ");
  assert!(!labels.contains("secret-token"));
  assert!(!labels.contains("private-password"));
  assert_eq!(
    routing_url("https://user:private-password@gateway.invalid/group").unwrap(),
    "https://gateway.invalid/group"
  );
}

#[test]
fn tailscale_preserves_existing_profile_name_and_checks_public_settings() {
  let connection = VpnConnection {
    connection_id: "tailscale-profile".into(),
    name: "Tailscale test".into(),
    settings: VpnSettings::Tailscale {
      hostname: None,
      accept_routes: true,
    },
  };
  let metadata = RuntimeMetadata::for_connection(&connection).unwrap();
  let mut previous_key = owner().unwrap().as_os_str().as_encoded_bytes().to_vec();
  previous_key.push(0);
  previous_key.extend(connection.connection_id.as_bytes());
  assert_eq!(
    container_name(&metadata).unwrap(),
    format!("ctld-tailscale-{}", digest(&previous_key))
  );
  let mut changed = connection;
  let VpnSettings::Tailscale { accept_routes, .. } = &mut changed.settings else {
    unreachable!()
  };
  *accept_routes = false;
  assert_ne!(
    metadata.settings_key,
    RuntimeMetadata::for_connection(&changed)
      .unwrap()
      .settings_key
  );
}

#[test]
fn inspection_verifies_protocol_owner_profile_and_immutable_identity() {
  let metadata = RuntimeMetadata::for_connection(&connection()).unwrap();
  let original = inspection(&metadata, ID, true, "running");
  for (label, value) in [
    (LABEL_PROTOCOL, "0"),
    (LABEL_USER, "foreign-owner"),
    (LABEL_ID, "foreign-profile"),
  ] {
    let mut changed = original.clone();
    changed[0]["Config"]["Labels"][label] = value.into();
    assert!(
      parse_descriptor(
        Path::new("engine"),
        ID,
        &serde_json::to_vec(&changed).unwrap()
      )
      .is_err()
    );
  }
  assert!(
    parse_descriptor(
      Path::new("engine"),
      OTHER_ID,
      &serde_json::to_vec(&original).unwrap()
    )
    .is_err()
  );
  let mut legacy = original.clone();
  legacy[0]["Config"]["Labels"] = serde_json::json!({});
  let error = parse_descriptor(
    Path::new("engine"),
    ID,
    &serde_json::to_vec(&legacy).unwrap(),
  )
  .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::Unsupported);
  assert!(error.to_string().contains("recreate legacy"));
  let descriptor = parse_descriptor(
    Path::new("engine"),
    ID,
    &serde_json::to_vec(&original).unwrap(),
  )
  .unwrap();
  let status = descriptor.basic_status();
  assert_eq!(status.state, VpnState::Connected);
  assert_eq!(status.container_id.as_deref(), Some(ID));
  assert!(status.shared_container);
  assert_eq!(status.locally_connected, Some(false));
  let mut unready = original;
  unready[0]["State"]["Health"]["Status"] = "starting".into();
  let descriptor = parse_descriptor(
    Path::new("engine"),
    ID,
    &serde_json::to_vec(&unready).unwrap(),
  )
  .unwrap();
  assert_eq!(descriptor.basic_status().state, VpnState::Starting);
  assert_eq!(descriptor.basic_status().endpoint, None);
}

#[tokio::test]
async fn inventory_is_read_only_and_deduplicates_container_ids() {
  let fixture = Fixture::new();
  let metadata = RuntimeMetadata::for_connection(&connection()).unwrap();
  fixture.inspect(ID, &inspection(&metadata, ID, true, "running"));
  fs::write(
    fixture.root.join("inventory"),
    format!("{ID}\n{ID}\n{OTHER_ID}\n"),
  )
  .unwrap();
  let containers = list(&fixture.engine).await.unwrap();
  assert_eq!(containers.len(), 1);
  assert_eq!(containers[0].id, ID);
  assert_eq!(fixture.calls().len(), 3);
  assert!(
    fixture
      .calls()
      .iter()
      .all(|call| call.starts_with("ps ") || call.starts_with("container inspect "))
  );
}

#[tokio::test]
async fn inventory_rejects_unverified_protocol_and_owner_without_acquiring_interest() {
  let fixture = Fixture::new();
  let metadata = RuntimeMetadata::for_connection(&connection()).unwrap();
  let verified = inspection(&metadata, ID, true, "running");
  fs::write(fixture.root.join("inventory"), format!("{ID}\n")).unwrap();
  // The fixture intentionally ignores engine filters. Inspection must still
  // reject an unrelated or incompatible resource returned by that inventory.
  for (label, value) in [(LABEL_USER, "foreign-owner"), (LABEL_PROTOCOL, "2")] {
    let mut unverified = verified.clone();
    unverified[0]["Config"]["Labels"][label] = value.into();
    fixture.inspect(ID, &unverified);
    assert!(list(&fixture.engine).await.is_err());
  }
  let mut unverified = verified;
  unverified[0]["Config"]["Labels"] = serde_json::json!({ "io.ctl.service": "tailscale" });
  fixture.inspect(ID, &unverified);
  assert!(list(&fixture.engine).await.is_err());
  let calls = fixture.calls();
  assert!(
    calls
      .iter()
      .all(|call| call.starts_with("ps ") || call.starts_with("container inspect "))
  );
  for inventory in calls.iter().filter(|call| call.starts_with("ps ")) {
    assert!(inventory.contains("--filter label=io.ctl.vpn.protocol=1"));
    assert!(inventory.contains(&format!(
      "--filter label=io.ctl.vpn.user={}",
      namespace().unwrap()
    )));
  }
}

#[tokio::test]
async fn inventory_limit_fails_instead_of_silently_hiding_connections() {
  let fixture = Fixture::new();
  let mut inventory = String::new();
  for id in 0..=MAX_CONTAINERS {
    writeln!(inventory, "{id:064x}").unwrap();
  }
  fs::write(fixture.root.join("inventory"), inventory).unwrap();
  assert!(
    list(&fixture.engine)
      .await
      .unwrap_err()
      .to_string()
      .contains("safety limit")
  );
  assert_eq!(fixture.calls().len(), 1);
}

#[tokio::test]
async fn engine_outage_is_not_reported_as_a_missing_container() {
  let fixture = Fixture::new();
  assert!(inspect_named(&fixture.engine, ID).await.unwrap().is_none());
  fs::write(fixture.root.join("unavailable"), "").unwrap();
  assert!(inspect_named(&fixture.engine, ID).await.is_err());
}

#[tokio::test]
async fn dropping_one_interest_does_not_stop_or_remove_the_shared_container() {
  let fixture = Fixture::new();
  let first = heartbeat(&fixture.engine, ID).await.unwrap();
  let second = heartbeat(&fixture.engine, ID).await.unwrap();
  drop(first);
  sleep(HEARTBEAT_INTERVAL + Duration::from_millis(150)).await;
  drop(second);
  let calls = fixture.calls();
  assert!(calls.len() >= 3);
  assert!(
    calls
      .iter()
      .all(|call| call == &format!("exec {ID} /bin/sh /run/ctl/heartbeat.sh"))
  );
  assert!(heartbeat(&fixture.engine, "mutable-name").await.is_err());
}

#[tokio::test]
async fn initial_heartbeat_waits_for_entrypoint_bootstrap() {
  let fixture = Fixture::new();
  fs::write(fixture.root.join("heartbeat-failed"), "").unwrap();
  let marker = fixture.root.join("heartbeat-failed");
  let bootstrap = tokio::spawn(async move {
    sleep(Duration::from_millis(150)).await;
    fs::remove_file(marker).unwrap();
  });
  let interest = heartbeat(&fixture.engine, ID).await.unwrap();
  bootstrap.await.unwrap();
  assert!(fixture.calls().len() >= 2);
  drop(interest);
}

#[tokio::test]
async fn explicit_cleanup_finishes_before_returning() {
  let fixture = Fixture::new();
  let metadata = RuntimeMetadata::for_connection(&connection()).unwrap();
  let name = container_name(&metadata).unwrap();
  fixture.inspect(&name, &inspection(&metadata, ID, false, "created"));
  let mut reservation = CreationGuard::new(fixture.engine.clone(), name, "creator-test".into());
  reservation.cleanup().await;
  assert!(
    fixture
      .calls()
      .iter()
      .any(|call| call == &format!("rm {ID}"))
  );
  let calls = fixture.calls().len();
  drop(reservation);
  sleep(Duration::from_millis(100)).await;
  assert_eq!(fixture.calls().len(), calls);
}

#[tokio::test]
async fn cancellation_removes_only_our_unstarted_immutable_reservation() {
  let fixture = Fixture::new();
  let metadata = RuntimeMetadata::for_connection(&connection()).unwrap();
  let name = container_name(&metadata).unwrap();
  fixture.inspect(&name, &inspection(&metadata, ID, false, "created"));
  drop(CreationGuard::new(
    fixture.engine.clone(),
    name.clone(),
    "creator-test".into(),
  ));
  timeout(Duration::from_secs(2), async {
    while !fixture
      .calls()
      .iter()
      .any(|call| call == &format!("rm {ID}"))
    {
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .unwrap();
  assert!(fixture.calls().iter().all(|call| !call.contains("--force")));
  for (running, state, token) in [
    (true, "running", "creator-test"),
    (false, "exited", "creator-test"),
    (false, "created", "foreign-creator"),
  ] {
    fs::write(fixture.root.join("calls"), "").unwrap();
    fixture.inspect(&name, &inspection(&metadata, ID, running, state));
    drop(CreationGuard::new(
      fixture.engine.clone(),
      name.clone(),
      token.into(),
    ));
    sleep(Duration::from_millis(100)).await;
    assert!(!fixture.calls().iter().any(|call| call.starts_with("rm ")));
  }
}

#[test]
fn cancellation_without_an_async_runtime_is_safe() {
  drop(CreationGuard::new(
    PathBuf::from("unused-engine"),
    "unused-name".into(),
    "unused-token".into(),
  ));
}

struct Process(Child);
impl Process {
  fn running(&mut self) -> bool {
    self.0.try_wait().unwrap().is_none()
  }
  fn stop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}
impl Drop for Process {
  fn drop(&mut self) {
    self.stop();
  }
}

struct WatchdogFixture {
  files: Fixture,
  target: Process,
  watchdog: Process,
}

impl WatchdogFixture {
  fn new() -> Self {
    let files = Fixture::new();
    fs::write(files.root.join("heartbeat.sh"), HEARTBEAT_SCRIPT).unwrap();
    fs::write(files.root.join("watchdog.sh"), WATCHDOG_SCRIPT).unwrap();
    fs::write(files.root.join("clock"), "100.00 0.00\n").unwrap();
    let target = Process(
      Command::new("/bin/sh")
        .args(["-c", "trap 'exit 0' TERM; while :; do sleep 0.1; done"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap(),
    );
    let watchdog = Process(
      Self::command(&files)
        .arg(files.root.join("watchdog.sh"))
        .arg(target.0.id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap(),
    );
    let fixture = Self {
      files,
      target,
      watchdog,
    };
    fixture.wait_marker(115);
    fixture
  }

  fn command(files: &Fixture) -> Command {
    let mut command = Command::new("/bin/sh");
    command
      .env("CTLD_HEARTBEAT_DIR", files.root.join("heartbeats"))
      .env("CTLD_HEARTBEAT_CLOCK_FILE", files.root.join("clock"));
    command
  }

  fn set_clock(&self, seconds: u64) {
    fs::write(
      self.files.root.join("next-clock"),
      format!("{seconds}.00 0.00\n"),
    )
    .unwrap();
    fs::rename(
      self.files.root.join("next-clock"),
      self.files.root.join("clock"),
    )
    .unwrap();
  }

  fn wait_marker(&self, deadline: u64) {
    let limit = Instant::now() + Duration::from_secs(3);
    while !self
      .files
      .root
      .join(format!("heartbeats/beats/{deadline}"))
      .exists()
    {
      assert!(Instant::now() < limit, "heartbeat marker was not published");
      std::thread::sleep(Duration::from_millis(10));
    }
  }

  fn sender(&self) -> Process {
    Process(
      Self::command(&self.files)
        .args(["-c", "while /bin/sh \"$1\"; do sleep 0.1; done", "sender"])
        .arg(self.files.root.join("heartbeat.sh"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap(),
    )
  }

  fn wait_exit(&mut self) {
    let limit = Instant::now() + Duration::from_secs(3);
    while self.target.running() {
      assert!(
        Instant::now() < limit,
        "watchdog did not stop the entrypoint after the last heartbeat expired"
      );
      std::thread::sleep(Duration::from_millis(20));
    }
    self.watchdog.0.wait().unwrap();
  }
}

#[test]
fn watchdog_accepts_two_daemons_and_expires_only_after_the_last_sender_stops() {
  let mut fixture = WatchdogFixture::new();
  let mut first = fixture.sender();
  let mut second = fixture.sender();
  fixture.set_clock(105);
  fixture.wait_marker(120);
  first.stop();
  fixture.set_clock(110);
  fixture.wait_marker(125);
  assert!(fixture.target.running());
  second.stop();
  // Reap the sender, then let any shell child already executing its final beat
  // finish before advancing the injected monotonic clock.
  std::thread::sleep(Duration::from_millis(200));
  fixture.set_clock(124);
  std::thread::sleep(Duration::from_millis(1100));
  assert!(fixture.target.running());
  fixture.set_clock(125);
  fixture.wait_exit();
}

#[test]
fn closed_stdin_does_not_stop_a_container_during_startup_grace() {
  let mut fixture = WatchdogFixture::new();
  fixture.set_clock(114);
  std::thread::sleep(Duration::from_millis(1100));
  assert!(fixture.target.running());
  fixture.set_clock(115);
  fixture.wait_exit();
}

#[test]
fn heartbeat_cannot_acknowledge_renewal_after_shutdown_is_claimed() {
  let fixture = Fixture::new();
  fs::create_dir_all(fixture.root.join("heartbeats/closing")).unwrap();
  fs::write(fixture.root.join("heartbeat.sh"), HEARTBEAT_SCRIPT).unwrap();
  fs::write(fixture.root.join("clock"), "100.00 0.00\n").unwrap();
  assert!(
    !WatchdogFixture::command(&fixture)
      .arg(fixture.root.join("heartbeat.sh"))
      .status()
      .unwrap()
      .success()
  );
  assert!(!fixture.root.join("heartbeats/beats").exists());
}

#[test]
fn watchdog_does_not_reuse_stale_time_after_the_clock_source_disappears() {
  let mut fixture = WatchdogFixture::new();
  assert!(fixture.target.running());
  fs::remove_file(fixture.files.root.join("clock")).unwrap();
  fixture.wait_exit();
}
