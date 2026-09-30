use super::*;
use serde_json::json;

fn running(peers: Value) -> Value {
  let mut status = json!({
    "BackendState": "Running",
    "Self": {"ID": "n-self"},
  });
  status["Peer"] = peers;
  status
}

fn peer(id: &str) -> Value {
  json!({
    "ID": id,
    "HostName": "work station",
    "DNSName": "work-station.example.ts.net.",
    "TailscaleIPs": ["100.100.1.2", "fd7a:115c:a1e0::2"],
    "Online": false,
    "OS": "linux",
    "InNetworkMap": true,
  })
}

#[test]
fn parses_stable_ids_and_offline_peers_without_using_map_keys_as_identity() {
  let discovery = parse_status(&running(json!({"nodekey:rotates": peer("n-stable")})));
  assert_eq!(discovery.state, DiscoveryState::Available);
  assert!(discovery.warnings.is_empty());
  assert_eq!(
    discovery.devices,
    vec![TailscaleDevice {
      node_id: "n-stable".into(),
      name: "work station".into(),
      dns_name: Some("work-station.example.ts.net".into()),
      addresses: vec!["100.100.1.2".into(), "fd7a:115c:a1e0::2".into()],
      online: Some(false),
      os: Some("linux".into()),
    }],
  );
}

#[test]
fn omits_self_expired_and_removed_peers_and_deduplicates_stable_ids() {
  let mut expired = peer("n-expired");
  expired["Expired"] = json!(true);
  let mut removed = peer("n-removed");
  removed["InNetworkMap"] = json!(false);
  let discovery = parse_status(&running(json!({
    "self": peer("n-self"),
    "expired": expired,
    "removed": removed,
    "first": peer("n-other"),
    "duplicate": peer("n-other"),
  })));
  assert_eq!(discovery.devices.len(), 1);
  assert_eq!(discovery.devices[0].node_id, "n-other");
}

#[test]
fn hides_sharee_nodes_but_keeps_shared_in_peers_like_tailscale_status() {
  // ShareeNode identifies a recipient of one of our shared nodes, not a node
  // shared with us. The official status CLI hides these inbound-only entries.
  let mut recipient = peer("n-sharee");
  recipient["ShareeNode"] = json!(true);
  let mut shared = peer("n-shared");
  shared["UserID"] = json!(123);
  let discovery = parse_status(&running(json!({"sharee": recipient, "shared": shared})));
  assert_eq!(discovery.devices.len(), 1);
  assert_eq!(discovery.devices[0].node_id, "n-shared");
}

#[test]
fn optional_peer_fields_can_be_absent() {
  let discovery = parse_status(&running(json!({
    "minimal": {"ID": "n-minimal", "TailscaleIPs": ["100.100.1.2"]},
  })));
  assert_eq!(discovery.devices.len(), 1);
  let device = &discovery.devices[0];
  assert_eq!(device.name, "100.100.1.2");
  assert_eq!(device.dns_name, None);
  assert_eq!(device.os, None);
  assert_eq!(device.online, None);
}

#[test]
fn invalid_identity_or_addresses_do_not_hide_other_peers() {
  let discovery = parse_status(&running(json!({
    "valid": peer("n-valid"),
    "missing_id": {"TailscaleIPs": ["100.100.1.2"]},
    "unsafe_id": {"ID": "n\nunsafe", "TailscaleIPs": ["100.100.1.2"]},
    "missing_address": {"ID": "n-no-address"},
    "unsafe_address": {"ID": "n-unsafe", "TailscaleIPs": ["-oProxyCommand=evil", "127.0.0.1"]},
    "wrong_type": false,
  })));
  assert_eq!(discovery.devices.len(), 1);
  assert_eq!(discovery.devices[0].node_id, "n-valid");
  assert_eq!(discovery.warnings.len(), 1);
}

#[test]
fn unsafe_dns_and_display_values_fall_back_to_valid_ip() {
  let mut input = peer("n-valid");
  input["HostName"] = json!("bad\u{0000}name");
  input["DNSName"] = json!("-oProxyCommand=evil");
  input["OS"] = json!("linux\nescape");
  input["TailscaleIPs"] = json!(["not an ip", "0.0.0.0", "::1", "224.0.0.1", "100.100.1.2"]);
  let device = parse_peer(&input).unwrap();
  assert_eq!(device.name, "100.100.1.2");
  assert_eq!(device.dns_name, None);
  assert_eq!(device.os, None);
  assert_eq!(device.addresses, vec!["100.100.1.2"]);
}

#[test]
fn recognizes_stopped_login_and_unknown_backend_states() {
  for (backend, state) in [
    ("Stopped", DiscoveryState::NotRunning),
    ("Starting", DiscoveryState::NotRunning),
    ("NoState", DiscoveryState::NotRunning),
    ("NeedsLogin", DiscoveryState::NeedsLogin),
    ("NeedsMachineAuth", DiscoveryState::NeedsLogin),
    ("Unknown", DiscoveryState::Error),
  ] {
    let discovery = parse_status(&json!({"BackendState": backend, "Peer": {"x": peer("n-x")}}));
    assert_eq!(discovery.state, state, "{backend}");
    assert!(discovery.devices.is_empty());
    assert_eq!(discovery.warnings.len(), 1);
  }
}

#[test]
fn empty_tailnet_is_available_but_invalid_peer_collection_is_an_error() {
  for peers in [Value::Null, json!({})] {
    let discovery = parse_status(&running(peers));
    assert_eq!(discovery.state, DiscoveryState::Available);
    assert!(discovery.devices.is_empty());
  }
  assert_eq!(
    parse_status(&running(json!([]))).state,
    DiscoveryState::Error
  );
}

#[test]
fn malformed_json_and_process_failures_produce_nonfatal_discovery_errors() {
  for (success, stdout, stderr, state) in [
    (true, "not json", "", DiscoveryState::Error),
    (
      false,
      "",
      "failed to connect to local tailscaled",
      DiscoveryState::NotRunning,
    ),
    (false, "", "not logged in", DiscoveryState::NeedsLogin),
    (false, "", "unexpected error", DiscoveryState::Error),
    (
      false,
      r#"{"BackendState":"Stopped"}"#,
      "",
      DiscoveryState::NotRunning,
    ),
    (
      false,
      r#"{"BackendState":"Running"}"#,
      "unexpected error",
      DiscoveryState::Error,
    ),
  ] {
    let discovery = discovery_from_output(&StatusOutput {
      success,
      stdout: stdout.as_bytes().to_vec(),
      stderr: stderr.as_bytes().to_vec(),
    });
    assert_eq!(discovery.state, state);
    assert!(discovery.devices.is_empty());
  }
}

#[test]
fn app_bundle_fallback_does_not_require_path_or_shell_expansion() {
  let directory = Path::new("/Users/a user/Applications");
  assert_eq!(
    cli_candidates(None, &mac_app_candidates(directory)),
    vec![
      directory.join("Tailscale.app/Contents/MacOS/Tailscale"),
      directory.join("Tailscale.app/Contents/MacOS/tailscale"),
    ],
  );
}

#[test]
fn candidate_resolution_preserves_path_order_and_avoids_duplicates_and_relative_paths() {
  let first = std::env::temp_dir().join("first bin");
  let second = std::env::temp_dir().join("second bin");
  let search_path = std::env::join_paths([&first, Path::new("relative"), &second]).unwrap();
  let filename = if cfg!(windows) {
    "tailscale.exe"
  } else {
    "tailscale"
  };
  assert_eq!(
    cli_candidates(Some(&search_path), &[first.join(filename)]),
    vec![first.join(filename), second.join(filename)],
  );
}

#[tokio::test]
async fn output_reads_are_bounded() {
  assert_eq!(read_limited(&b"abc"[..], 3).await.unwrap(), b"abc");
  assert_eq!(
    read_limited(&b"abcd"[..], 3).await.unwrap_err().kind(),
    io::ErrorKind::InvalidData,
  );
}

#[tokio::test]
async fn missing_client_is_reported_without_failure() {
  let discovery = discover(&[], STATUS_TIMEOUT).await;
  assert_eq!(discovery.state, DiscoveryState::NotInstalled);
  assert!(discovery.devices.is_empty());
}

#[cfg(unix)]
mod processes {
  use std::fs;
  use std::os::unix::fs::PermissionsExt as _;

  use super::*;

  // Keep file creation through execution serialized; concurrent fork/exec can
  // inherit a fixture's writable descriptor before fs::write closes it.
  static FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

  struct Fixture {
    directory: PathBuf,
    _guard: tokio::sync::MutexGuard<'static, ()>,
  }

  impl Fixture {
    async fn new(script: &str) -> Self {
      let guard = FIXTURE_LOCK.lock().await;
      let directory = std::env::temp_dir().join(format!("rmux tailscale {}", uuid::Uuid::new_v4()));
      let executable = mac_app_candidates(&directory).remove(0);
      fs::create_dir_all(executable.parent().unwrap()).unwrap();
      fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
      fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
      Self {
        directory,
        _guard: guard,
      }
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ignored = fs::remove_dir_all(&self.directory);
    }
  }

  #[tokio::test]
  async fn executes_app_bundle_with_spaces_in_cli_mode_without_path() {
    let fixture = Fixture::new(concat!(
      "[ \"$TAILSCALE_BE_CLI\" = 1 ] || exit 11\n",
      "[ \"$*\" = 'status --json --peers=true' ] || exit 12\n",
      "printf '%s' '{\"BackendState\":\"Running\",\"Peer\":{}}'",
    ))
    .await;
    let paths = cli_candidates(None, &mac_app_candidates(&fixture.directory));
    let discovery = discover(&paths, STATUS_TIMEOUT).await;
    assert_eq!(discovery.state, DiscoveryState::Available);
  }

  #[tokio::test]
  async fn missing_path_candidate_falls_back_to_application() {
    let fixture = Fixture::new("printf '%s' '{\"BackendState\":\"NeedsLogin\"}'\nexit 1").await;
    let search_path = std::env::join_paths([fixture.directory.join("absent")]).unwrap();
    let paths = cli_candidates(Some(&search_path), &mac_app_candidates(&fixture.directory));
    assert_eq!(
      discover(&paths, STATUS_TIMEOUT).await.state,
      DiscoveryState::NeedsLogin
    );
  }

  #[tokio::test]
  async fn stalled_client_is_terminated_on_timeout() {
    let fixture = Fixture::new("exec /bin/sleep 30").await;
    let executable = mac_app_candidates(&fixture.directory).remove(0);
    let error = read_status(&executable, Duration::from_millis(20))
      .await
      .err()
      .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
  }
}
